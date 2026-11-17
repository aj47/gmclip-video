//! VP9 on VideoToolbox (H-03, SPEC §6.2): Apple's supplemental decoder, a synthesised
//! `vpcC`, superframe splitting and `show_existing_frame`.
//!
//! The VP9 field readers here are a **temporary shim** until Lane B's allowed-fields
//! readers land (B-03, `codec/ALLOWED_FIELDS.md`). They read only framing and the
//! uncompressed-header fields needed to route frames: the superframe index,
//! `frame_marker`, `profile`, `show_existing_frame` / `frame_to_show_map_idx`,
//! `frame_type`, `show_frame`, `error_resilient_mode`, `intra_only`,
//! `reset_frame_context`, `refresh_frame_flags`, and on key frames the bit depth and
//! colour config (for `vpcC`) and the frame size. No coded picture data is read.
//!
//! Decoding: each superframe is split and every sub-frame is decoded on its own,
//! synchronously, so the decoder's output for a hidden frame (`show_frame = 0`, the
//! alt-ref) is captured. Decoded pictures are kept per reference slot following
//! `refresh_frame_flags`; a `show_existing_frame` sub-frame is not sent to
//! VideoToolbox and repeats the picture in `frame_to_show_map_idx` (the libvpx
//! behaviour ffmpeg's decoder also has). Each packet yields exactly one picture.

use std::collections::VecDeque;
use std::sync::Arc;

use gmclip_gpu::apple::{
    CodecConfig, DecodeFlags, PixelBuffer, PlaneFormat, VtDecoder, VtDecoderConfig,
};

use super::media_err;
use crate::decode::{
    DecodedFrame, DecoderCaps, EncodedSample, FrameImage, MediaError, SubmitFlags, VideoDecoder,
};

fn bad(step: &'static str) -> MediaError {
    MediaError {
        step,
        frame: None,
        source_id: None,
        os_status: None,
    }
}

/// Splits a VP9 packet at its superframe index (VP9 bitstream spec Annex B). A packet
/// without a valid index is one frame.
pub fn split_superframe(data: &[u8]) -> Vec<&[u8]> {
    let Some(&last) = data.last() else {
        return vec![];
    };
    if last & 0xe0 != 0xc0 {
        return vec![data];
    }
    let frames = usize::from(last & 7) + 1;
    let mag = usize::from((last >> 3) & 3) + 1;
    let index_len = 2 + mag * frames;
    if data.len() < index_len || data[data.len() - index_len] != last {
        return vec![data];
    }
    let idx = &data[data.len() - index_len + 1..data.len() - 1];
    let mut out = Vec::with_capacity(frames);
    let mut pos = 0usize;
    for f in 0..frames {
        let mut size = 0usize;
        for b in 0..mag {
            size |= usize::from(idx[f * mag + b]) << (8 * b);
        }
        let end = pos.saturating_add(size);
        if end > data.len() - index_len {
            return vec![data]; // malformed index: hand the packet over whole
        }
        out.push(&data[pos..end]);
        pos = end;
    }
    out
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32, MediaError> {
        let byte = *self
            .d
            .get(self.pos / 8)
            .ok_or_else(|| bad("vp9_header_truncated"))?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(u32::from(b))
    }

    fn bits(&mut self, n: u32) -> Result<u32, MediaError> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }
}

/// The routing fields of one VP9 frame's uncompressed header.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vp9Header {
    pub profile: u8,
    /// `Some(slot)` for a `show_existing_frame` frame.
    pub show_existing: Option<u8>,
    pub key_frame: bool,
    pub show_frame: bool,
    pub intra_only: bool,
    /// Reference slots this frame refreshes (all 8 for a key frame).
    pub refresh: u8,
    /// Key frames only: bit depth, colour space (`color_space` code), full range,
    /// and the frame size.
    pub bit_depth: u8,
    pub color_space: u8,
    pub full_range: bool,
    /// `subsampling_x << 1 | subsampling_y` (0b11 = 4:2:0).
    pub subsampling: u8,
    pub width: u32,
    pub height: u32,
}

fn sync_code(b: &mut Bits<'_>) -> Result<(), MediaError> {
    if b.bits(24)? != 0x49_83_42 {
        return Err(bad("vp9_bad_sync_code"));
    }
    Ok(())
}

fn color_config(b: &mut Bits<'_>, h: &mut Vp9Header) -> Result<(), MediaError> {
    h.bit_depth = if h.profile >= 2 {
        if b.bit()? == 1 { 12 } else { 10 }
    } else {
        8
    };
    h.color_space = b.bits(3)? as u8;
    if h.color_space != 7 {
        h.full_range = b.bit()? == 1;
        if h.profile == 1 || h.profile == 3 {
            h.subsampling = b.bits(2)? as u8; // subsampling_x, subsampling_y
            if b.bit()? != 0 {
                return Err(bad("vp9_reserved_bit_set"));
            }
        } else {
            h.subsampling = 0b11;
        }
    } else {
        h.full_range = true;
        h.subsampling = 0;
        if (h.profile == 1 || h.profile == 3) && b.bit()? != 0 {
            return Err(bad("vp9_reserved_bit_set"));
        }
    }
    Ok(())
}

/// Parses the routing fields of a VP9 frame header.
pub fn parse_header(frame: &[u8]) -> Result<Vp9Header, MediaError> {
    let mut b = Bits { d: frame, pos: 0 };
    if b.bits(2)? != 2 {
        return Err(bad("vp9_bad_frame_marker"));
    }
    let lo = b.bit()?;
    let hi = b.bit()?;
    let mut h = Vp9Header {
        profile: ((hi << 1) | lo) as u8,
        ..Default::default()
    };
    if h.profile == 3 && b.bit()? != 0 {
        return Err(bad("vp9_reserved_bit_set")); // libvpx: profile > 3, refused
    }
    if b.bit()? == 1 {
        h.show_existing = Some(b.bits(3)? as u8);
        return Ok(h);
    }
    h.key_frame = b.bit()? == 0;
    h.show_frame = b.bit()? == 1;
    let error_resilient = b.bit()? == 1;
    if h.key_frame {
        sync_code(&mut b)?;
        color_config(&mut b, &mut h)?;
        h.width = b.bits(16)? + 1;
        h.height = b.bits(16)? + 1;
        h.refresh = 0xff;
        return Ok(h);
    }
    h.intra_only = if h.show_frame { false } else { b.bit()? == 1 };
    if !error_resilient {
        b.bits(2)?; // reset_frame_context
    }
    if h.intra_only {
        sync_code(&mut b)?;
        if h.profile > 0 {
            color_config(&mut b, &mut h)?;
        } else {
            h.bit_depth = 8;
        }
    }
    h.refresh = b.bits(8)? as u8;
    Ok(h)
}

/// A `vpcC` box payload (version 1 FullBox header included) for VideoToolbox's
/// format description, from the first key frame's header and the container's
/// colour tags (ISO/IEC 23091-2 codes; `None` → 2, unspecified).
pub fn vpcc(
    key: &Vp9Header,
    primaries: Option<u8>,
    transfer: Option<u8>,
    matrix: Option<u8>,
) -> Vec<u8> {
    // Level 0 = "unspecified". Chroma: 0 = 4:2:0 (vertical siting, VP9's default),
    // 2 = 4:2:2, 3 = 4:4:4 (from subsampling_x/y on profiles 1 and 3).
    let chroma = match key.subsampling {
        0b10 => 2u8,
        0b00 => 3,
        _ => 0,
    };
    vec![
        1,
        0,
        0,
        0, // version 1, flags 0
        key.profile,
        0,
        (key.bit_depth << 4) | (chroma << 1) | u8::from(key.full_range),
        primaries.unwrap_or(2),
        transfer.unwrap_or(2),
        matrix.unwrap_or(2),
        0,
        0, // codecInitializationDataSize
    ]
}

/// VideoToolbox VP9 decode with superframe splitting and `show_existing_frame`.
/// Synchronous; outputs are in decode order (VP9 has no reordering).
#[derive(Debug)]
pub struct VtVp9Decoder {
    vt: VtDecoder,
    caps: DecoderCaps,
    slots: [Option<Arc<PixelBuffer>>; 8],
    out: VecDeque<Result<DecodedFrame, MediaError>>,
    seq: u64,
    /// The chain's key-frame format (size, depth, profile, range); a later key frame
    /// that differs is an error (the session was made for this one).
    key: Option<(u32, u32, u8, u8, bool)>,
}

impl VtVp9Decoder {
    /// The output format for a key frame's header: 4:2:0 only (profiles 0 and 2);
    /// 8-bit video or full range, 10-bit video range.
    pub fn output_format(key: &Vp9Header) -> Result<PlaneFormat, MediaError> {
        match (key.profile, key.bit_depth, key.full_range) {
            (0, 8, false) => Ok(PlaneFormat::Nv12Video),
            (0, 8, true) => Ok(PlaneFormat::Nv12Full),
            (2, 10, false) => Ok(PlaneFormat::P010Video),
            _ => Err(bad("vp9_unsupported_format")),
        }
    }

    /// `vpcc` from [`vpcc`]; `output` from [`Self::output_format`]. Packets carrying
    /// only hidden frames (Matroska invisible blocks, or streams remuxed through
    /// `vp9_superframe_split`) are unsupported: every packet must show one picture.
    pub fn new(
        vpcc: Vec<u8>,
        width: u32,
        height: u32,
        output: PlaneFormat,
        timescale: i32,
    ) -> Result<Self, MediaError> {
        let vt = VtDecoder::new(&VtDecoderConfig {
            codec: CodecConfig::Vp9 { vpcc },
            width,
            height,
            output,
            require_hardware: false,
            timescale,
        })
        .map_err(|e| media_err("vt_vp9_session_create", None, e))?;
        let caps = DecoderCaps {
            hardware: vt.is_hardware(),
            max_sessions: 3,
        };
        Ok(Self {
            vt,
            caps,
            slots: Default::default(),
            out: VecDeque::new(),
            seq: 0,
            key: None,
        })
    }

    fn decode_sub(
        &mut self,
        data: &[u8],
        s: &EncodedSample<'_>,
    ) -> Result<Option<Arc<PixelBuffer>>, MediaError> {
        let frame = Some(u64::from(s.sample.0));
        let tag = self.seq;
        self.seq += 1;
        self.vt
            .decode(data, s.pts, s.dts, None, tag, DecodeFlags::default())
            .map_err(|e| media_err("vt_vp9_decode_submit", frame, e))?;
        let mut got = None;
        let mut seen = false;
        for attempt in 0..2 {
            if attempt == 1 {
                if seen {
                    break;
                }
                // Not delivered inside the decode call: finish delayed frames once.
                self.vt
                    .flush()
                    .map_err(|e| media_err("vt_vp9_flush", frame, e))?;
            }
            while let Some(f) = self.vt.take() {
                debug_assert_eq!(f.tag, tag, "synchronous VP9 decode: one record per call");
                if f.tag != tag {
                    continue;
                }
                seen = true;
                if f.info_flags & 0x2 != 0 {
                    return Err(MediaError {
                        step: "vt_vp9_frame_dropped",
                        frame,
                        source_id: None,
                        os_status: None,
                    });
                }
                if f.status != 0 {
                    return Err(MediaError {
                        step: "vt_vp9_decode",
                        frame,
                        source_id: None,
                        os_status: Some(i64::from(f.status)),
                    });
                }
                got = f.image.map(Arc::new);
            }
        }
        Ok(got)
    }

    fn submit_inner(&mut self, s: &EncodedSample<'_>, no_output: bool) -> Result<(), MediaError> {
        let frame = Some(u64::from(s.sample.0));
        let mut shown: Option<Arc<PixelBuffer>> = None;
        for sub in split_superframe(s.data) {
            let h = parse_header(sub).map_err(|mut e| {
                e.frame = frame;
                e
            })?;
            if h.key_frame {
                let k = (h.width, h.height, h.profile, h.bit_depth, h.full_range);
                match self.key {
                    None => self.key = Some(k),
                    Some(have) if have != k => {
                        return Err(MediaError {
                            step: "vp9_key_frame_format_changed",
                            frame,
                            source_id: None,
                            os_status: None,
                        });
                    }
                    _ => {}
                }
            } else if self.key.is_none() && h.show_existing.is_none() {
                return Err(MediaError {
                    step: "vp9_chain_starts_without_key_frame",
                    frame,
                    source_id: None,
                    os_status: None,
                });
            }
            if let Some(slot) = h.show_existing {
                let pic = self.slots[usize::from(slot)].clone().ok_or(MediaError {
                    step: "vp9_show_existing_empty_slot",
                    frame,
                    source_id: None,
                    os_status: None,
                })?;
                shown = Some(pic);
                continue;
            }
            let pic = self.decode_sub(sub, s)?;
            if let Some(pic) = &pic {
                for k in 0..8 {
                    if h.refresh & (1 << k) != 0 {
                        self.slots[k] = Some(pic.clone());
                    }
                }
            } else if h.refresh != 0 || h.show_frame {
                return Err(MediaError {
                    step: "vt_vp9_no_picture",
                    frame,
                    source_id: None,
                    os_status: None,
                });
            }
            if h.show_frame {
                shown = pic;
            }
        }
        if !no_output {
            let pic = shown.ok_or(MediaError {
                step: "vp9_packet_shows_nothing",
                frame,
                source_id: None,
                os_status: None,
            })?;
            self.out.push_back(Ok(DecodedFrame {
                sample: s.sample,
                pts: s.pts,
                image: FrameImage::Apple(pic),
            }));
        }
        Ok(())
    }
}

impl VideoDecoder for VtVp9Decoder {
    fn submit(&mut self, s: &EncodedSample<'_>, flags: SubmitFlags) -> Result<(), MediaError> {
        self.submit_inner(s, flags.no_output)
    }

    fn poll(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        self.out.pop_front().transpose()
    }

    fn flush(&mut self) -> Result<(), MediaError> {
        self.vt
            .flush()
            .map_err(|e| media_err("vt_vp9_flush", None, e))
    }

    fn caps(&self) -> &DecoderCaps {
        &self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superframe_split() {
        // Two frames of 3 and 2 bytes, 1-byte sizes: marker 0b110_00_001 = 0xc1.
        let pkt = [0x82, 1, 2, 0x86, 3, 0xc1, 3, 2, 0xc1];
        let f = split_superframe(&pkt);
        assert_eq!(f, vec![&pkt[0..3], &pkt[3..5]]);
        // No index: one frame.
        assert_eq!(split_superframe(&[0x82, 0x49]), vec![&[0x82u8, 0x49][..]]);
        // Index whose leading marker disagrees: one frame.
        let bad_idx = [0x82, 1, 2, 0x86, 3, 0x00, 3, 2, 0xc1];
        assert_eq!(split_superframe(&bad_idx).len(), 1);
        // Sizes past the data: one frame, never a panic.
        let over = [0x82, 0xc1, 9, 9, 0xc1];
        assert_eq!(split_superframe(&over).len(), 1);
    }

    #[test]
    fn header_fields() {
        // show_existing_frame of slot 5: 10 0 0 1 101 → 0b1000_1101 = 0x8d.
        let h = parse_header(&[0x8d]).unwrap();
        assert_eq!(h.show_existing, Some(5));
        // Key frame, profile 0, shown: 10 0 0 0 0 1 0 | sync | cs=2 (bt709) range 0 | size.
        // bits: 1000 0010 then 0x49 0x83 0x42 then 010 0 ... width-1 = 2559, height-1 = 1439.
        let mut bits = String::from("10000010");
        bits += "010010011000001101000010";
        bits += "0100"; // color_space 2, color_range 0
        bits += &format!("{:016b}{:016b}", 2559, 1439);
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = bits
            .as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect();
        let h = parse_header(&bytes).unwrap();
        assert!(h.key_frame && h.show_frame);
        assert_eq!((h.bit_depth, h.color_space, h.full_range), (8, 2, false));
        assert_eq!((h.width, h.height, h.refresh), (2560, 1440, 0xff));
        let c = vpcc(&h, Some(1), Some(1), Some(1));
        assert_eq!(c, vec![1, 0, 0, 0, 0, 0, 0x80, 1, 1, 1, 0, 0]);
        // Truncated input is an error, not a panic.
        assert!(parse_header(&[0x82]).is_err());
    }
}
