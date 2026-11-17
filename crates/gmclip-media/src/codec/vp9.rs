//! VP9 allowed fields (SPEC §15.0, `codec/ALLOWED_FIELDS.md`): the superframe index
//! (Annex B of the VP9 bitstream specification), the start of the uncompressed header
//! up to the frame size (profile, show_existing_frame, frame type, colour config, frame
//! and render size), and `vpcC` synthesis from those fields. Parsing stops before the
//! loop-filter, quantisation and segmentation syntax; no compressed header or tile
//! data is read.

use super::bits::BitReader;
use super::{CodecError, ColourTags, Result};

/// Frames inside one VP9 packet: `(offset, size)` per frame. A packet with no superframe
/// index is one frame.
pub fn superframe(data: &[u8]) -> Result<Vec<(usize, usize)>> {
    let Some(&last) = data.last() else {
        return Err(CodecError::Truncated {
            field: "superframe",
            bit: 0,
        });
    };
    if last & 0xE0 != 0xC0 {
        return Ok(vec![(0, data.len())]);
    }
    let frames = usize::from(last & 0x07) + 1;
    let mag = usize::from((last >> 3) & 0x03) + 1;
    let index = 2 + mag * frames;
    if index > data.len() {
        return Ok(vec![(0, data.len())]);
    }
    let start = data.len() - index;
    // The index begins and ends with the same marker byte; otherwise the trailing byte
    // just happens to look like one and the packet is a single frame.
    if data.get(start) != Some(&last) {
        return Ok(vec![(0, data.len())]);
    }
    let mut out = Vec::with_capacity(frames);
    let mut at = 0usize;
    for k in 0..frames {
        let p = start + 1 + k * mag;
        let bytes = data.get(p..p + mag).ok_or(CodecError::Truncated {
            field: "frame_sizes",
            bit: p * 8,
        })?;
        let size = bytes
            .iter()
            .rev()
            .fold(0usize, |v, &b| (v << 8) | usize::from(b));
        let end = at
            .checked_add(size)
            .filter(|&e| e <= start)
            .ok_or(CodecError::Invalid {
                field: "frame_sizes",
                reason: "superframe sizes exceed the packet",
            })?;
        // A zero size carries no frame (libvpx skips such entries); bytes left after
        // the last frame are padding.
        if size > 0 {
            out.push((at, size));
        }
        at = end;
    }
    Ok(out)
}

/// VP9 `color_space` (spec §7.2.2).
pub const CS_UNKNOWN: u8 = 0;
pub const CS_RGB: u8 = 7;

/// The uncompressed-header fields kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub profile: u8,
    /// `show_existing_frame`: the packet only re-shows slot `frame_to_show_map_idx`.
    pub show_existing_frame: bool,
    pub frame_to_show: Option<u8>,
    pub key_frame: bool,
    pub show_frame: bool,
    pub intra_only: bool,
    /// From color_config (key and intra-only frames).
    pub bit_depth: Option<u8>,
    pub color_space: Option<u8>,
    pub full_range: Option<bool>,
    /// (subsampling_x, subsampling_y)
    pub subsampling: Option<(u8, u8)>,
    /// Coded frame size (key and intra-only frames).
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub render_size: Option<(u32, u32)>,
}

fn sync_code(r: &mut BitReader<'_>) -> Result<()> {
    if r.bits(24, "frame_sync_code")? != 0x49_8342 {
        return Err(CodecError::Invalid {
            field: "frame_sync_code",
            reason: "not 0x498342",
        });
    }
    Ok(())
}

fn reserved_zero(r: &mut BitReader<'_>) -> Result<()> {
    if r.flag("reserved_zero")? {
        return Err(CodecError::Invalid {
            field: "reserved_zero",
            reason: "set",
        });
    }
    Ok(())
}

fn color_config(r: &mut BitReader<'_>, h: &mut Header) -> Result<()> {
    let bit_depth = if h.profile >= 2 {
        if r.flag("ten_or_twelve_bit")? { 12 } else { 10 }
    } else {
        8
    };
    let cs = r.bits(3, "color_space")? as u8;
    let odd_profile = h.profile == 1 || h.profile == 3;
    let (full, ss) = if cs != CS_RGB {
        let full = r.flag("color_range")?;
        let ss = if odd_profile {
            let s = (
                r.bits(1, "subsampling_x")? as u8,
                r.bits(1, "subsampling_y")? as u8,
            );
            reserved_zero(r)?;
            if s == (1, 1) {
                return Err(CodecError::Invalid {
                    field: "subsampling",
                    reason: "4:2:0 needs profile 0 or 2",
                });
            }
            s
        } else {
            (1, 1)
        };
        (full, ss)
    } else {
        if !odd_profile {
            return Err(CodecError::Invalid {
                field: "color_space",
                reason: "RGB needs profile 1 or 3",
            });
        }
        reserved_zero(r)?;
        (true, (0, 0))
    };
    h.bit_depth = Some(bit_depth);
    h.color_space = Some(cs);
    h.full_range = Some(full);
    h.subsampling = Some(ss);
    Ok(())
}

fn frame_and_render_size(r: &mut BitReader<'_>, h: &mut Header) -> Result<()> {
    let w = r.bits(16, "frame_width_minus_1")? + 1;
    let hh = r.bits(16, "frame_height_minus_1")? + 1;
    h.width = Some(w);
    h.height = Some(hh);
    h.render_size = Some(if r.flag("render_and_frame_size_different")? {
        (
            r.bits(16, "render_width_minus_1")? + 1,
            r.bits(16, "render_height_minus_1")? + 1,
        )
    } else {
        (w, hh)
    });
    Ok(())
}

/// Parse the start of one frame's uncompressed header (one frame: split superframes
/// first).
pub fn parse_header(frame: &[u8]) -> Result<Header> {
    let mut r = BitReader::new(frame);
    if r.bits(2, "frame_marker")? != 2 {
        return Err(CodecError::Invalid {
            field: "frame_marker",
            reason: "not 2",
        });
    }
    let low = r.bits(1, "profile_low_bit")?;
    let high = r.bits(1, "profile_high_bit")?;
    let mut h = Header {
        profile: ((high << 1) | low) as u8,
        ..Header::default()
    };
    if h.profile == 3 {
        reserved_zero(&mut r)?;
    }
    h.show_existing_frame = r.flag("show_existing_frame")?;
    if h.show_existing_frame {
        h.frame_to_show = Some(r.bits(3, "frame_to_show_map_idx")? as u8);
        h.show_frame = true;
        return Ok(h);
    }
    h.key_frame = !r.flag("frame_type")?;
    h.show_frame = r.flag("show_frame")?;
    let error_resilient = r.flag("error_resilient_mode")?;
    if h.key_frame {
        sync_code(&mut r)?;
        color_config(&mut r, &mut h)?;
        frame_and_render_size(&mut r, &mut h)?;
        return Ok(h);
    }
    h.intra_only = if h.show_frame {
        false
    } else {
        r.flag("intra_only")?
    };
    if !error_resilient {
        r.skip(2, "reset_frame_context")?;
    }
    if h.intra_only {
        sync_code(&mut r)?;
        if h.profile > 0 {
            color_config(&mut r, &mut h)?;
        } else {
            h.bit_depth = Some(8);
            h.color_space = Some(1); // CS_BT_601
            h.full_range = Some(false);
            h.subsampling = Some((1, 1));
        }
        r.skip(8, "refresh_frame_flags")?;
        frame_and_render_size(&mut r, &mut h)?;
    }
    Ok(h)
}

/// The matrix coefficients a VP9 `color_space` implies (ISO/IEC 23091-2).
pub fn matrix_for(color_space: u8) -> u8 {
    match color_space {
        1 | 3 => 6, // BT.601, SMPTE 170M
        2 => 1,     // BT.709
        4 => 7,     // SMPTE 240M
        5 => 9,     // BT.2020 non-constant
        7 => 0,     // RGB
        _ => 2,     // unknown, reserved: unspecified
    }
}

/// VP9 level (10 = 1.0 … 62 = 6.2) from luma picture size and luma sample rate, per the
/// WebM "VP9 levels" table; 0 when nothing fits. Bit rate, tiles and reference
/// constraints are not considered.
pub fn level(width: u32, height: u32, fps: f64) -> u8 {
    const LEVELS: [(u8, f64, u64); 14] = [
        (10, 829_440.0, 36_864),
        (11, 2_764_800.0, 73_728),
        (20, 4_608_000.0, 122_880),
        (21, 9_216_000.0, 245_760),
        (30, 20_736_000.0, 552_960),
        (31, 36_864_000.0, 983_040),
        (40, 83_558_400.0, 2_228_224),
        (41, 160_432_128.0, 2_228_224),
        (50, 311_951_360.0, 8_912_896),
        (51, 588_251_136.0, 8_912_896),
        (52, 1_176_502_272.0, 8_912_896),
        (60, 1_176_502_272.0, 35_651_584),
        (61, 2_353_004_544.0, 35_651_584),
        (62, 4_706_009_088.0, 35_651_584),
    ];
    let pic = u64::from(width) * u64::from(height);
    let rate = pic as f64 * fps;
    LEVELS
        .iter()
        .find(|(_, r, p)| pic <= *p && rate <= *r)
        .map_or(0, |l| l.0)
}

/// `fps` is the track's frame rate (its constant rate, else frames over duration); the
/// level considers picture size and luma sample rate only, not the table's maximum
/// picture breadth, bit rate or tiles (the tools/-made reference agrees on the corpus).
///
/// Synthesize the `vpcC` box payload (VP Codec ISO Media File Format Binding §2.3:
/// version 1, flags 0, profile, level, bitDepth|chromaSubsampling|videoFullRangeFlag,
/// colourPrimaries, transferCharacteristics, matrixCoefficients, no initialization
/// data) from a key frame's header, the frame rate and the container's colour tags.
/// Container tags win over the bitstream; absent ones are "unspecified" (2).
pub fn synthesize_vpcc(key: &Header, fps: f64, colour: &ColourTags) -> Result<[u8; 12]> {
    let missing = |f: &'static str| CodecError::Invalid {
        field: f,
        reason: "needs a key frame header",
    };
    let bit_depth = key.bit_depth.ok_or(missing("bit_depth"))?;
    let (sx, sy) = key.subsampling.ok_or(missing("subsampling"))?;
    let (w, h) = (
        key.width.ok_or(missing("width"))?,
        key.height.ok_or(missing("height"))?,
    );
    let chroma = match (sx, sy) {
        (1, 1) => {
            if colour.chroma_left == Some(true) {
                0
            } else {
                1
            }
        }
        (1, 0) => 2,
        (0, 0) => 3,
        _ => {
            return Err(CodecError::Unsupported {
                what: "VP9 4:4:0 has no vpcC chroma code",
            });
        }
    };
    let full = colour.full_range.or(key.full_range).unwrap_or(false);
    let matrix = colour
        .matrix
        .unwrap_or_else(|| matrix_for(key.color_space.unwrap_or(CS_UNKNOWN)));
    Ok([
        1,
        0,
        0,
        0,
        key.profile,
        level(w, h, fps),
        (bit_depth << 4) | (chroma << 1) | u8::from(full),
        colour.primaries.unwrap_or(2),
        colour.transfer.unwrap_or(2),
        matrix,
        0,
        0,
    ])
}

/// A `vpcC` record's fields.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Vpcc {
    pub profile: u8,
    pub level: u8,
    pub bit_depth: u8,
    pub chroma: u8,
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

/// Read a `vpcC` payload back (version 1). Used to round-trip the synthesis.
pub fn parse_vpcc(p: &[u8]) -> Result<Vpcc> {
    let b = |i: usize| {
        p.get(i).copied().ok_or(CodecError::Truncated {
            field: "vpcC",
            bit: i * 8,
        })
    };
    if b(0)? != 1 {
        return Err(CodecError::Unsupported {
            what: "vpcC version other than 1",
        });
    }
    let packed = b(6)?;
    let init = (usize::from(b(10)?) << 8) | usize::from(b(11)?);
    if p.len() < 12 + init {
        return Err(CodecError::Truncated {
            field: "codecInitializationData",
            bit: p.len() * 8,
        });
    }
    Ok(Vpcc {
        profile: b(4)?,
        level: b(5)?,
        bit_depth: packed >> 4,
        chroma: (packed >> 1) & 7,
        full_range: packed & 1 == 1,
        primaries: b(7)?,
        transfer: b(8)?,
        matrix: b(9)?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn superframe_split() {
        // two frames of 3 and 2 bytes, 1-byte sizes: marker 0b110_00_001 = 0xC1
        let p = [1, 2, 3, 4, 5, 0xC1, 3, 2, 0xC1];
        assert_eq!(superframe(&p).unwrap(), vec![(0, 3), (3, 2)]);
        // a lone trailing byte that looks like a marker is a plain frame
        assert_eq!(superframe(&[9, 9, 0xC1]).unwrap(), vec![(0, 3)]);
        // sizes that overrun
        assert!(superframe(&[1, 0xC1, 9, 9, 0xC1]).is_err());
        assert!(superframe(&[]).is_err());
    }

    #[test]
    fn key_frame_header() {
        // frame_marker 10, profile 0 (0,0), show_existing 0, frame_type 0 (key), show 1,
        // error_res 0, sync 0x498342, color_space 2 (BT.709), range 0,
        // width-1 = 1919, height-1 = 1079, render same
        let mut bits = String::from("10000010");
        bits += &format!("{:024b}", 0x49_8342);
        bits += "0100";
        bits += &format!("{:016b}{:016b}", 1919, 1079);
        bits += "0";
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = (0..bits.len() / 8)
            .map(|i| u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap())
            .collect();
        let h = parse_header(&bytes).unwrap();
        assert!(h.key_frame && h.show_frame);
        assert_eq!(
            (h.width, h.height, h.bit_depth, h.color_space),
            (Some(1920), Some(1080), Some(8), Some(2))
        );
        let v = synthesize_vpcc(&h, 30.0, &ColourTags::default()).unwrap();
        assert_eq!(v, [1, 0, 0, 0, 0, 40, 0x82, 2, 2, 1, 0, 0]);
        let back = parse_vpcc(&v).unwrap();
        assert_eq!(
            (back.profile, back.level, back.bit_depth, back.chroma),
            (0, 40, 8, 1)
        );
        assert_eq!(
            (back.full_range, back.primaries, back.transfer, back.matrix),
            (false, 2, 2, 1)
        );
        for n in 0..bytes.len() {
            assert!(parse_header(&bytes[..n]).is_err());
        }
    }

    #[test]
    fn levels() {
        assert_eq!(level(2560, 1440, 30.0), 50);
        assert_eq!(level(1920, 1080, 30.0), 40);
        assert_eq!(level(1920, 1080, 60.0), 41);
        assert_eq!(level(1280, 720, 30.0), 31);
        assert_eq!(level(16384, 16384, 120.0), 0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod known_answers {
    use super::*;
    use crate::codec::bits::BitWriter;

    fn key(profile: u8, f: impl FnOnce(&mut BitWriter)) -> Result<Header> {
        let mut w = BitWriter::default();
        w.put(2, 2)
            .put(u64::from(profile & 1), 1)
            .put(u64::from(profile >> 1), 1);
        if profile == 3 {
            w.flag(false);
        }
        w.flag(false)
            .flag(false)
            .flag(true)
            .flag(false)
            .put(0x49_8342, 24);
        f(&mut w);
        w.put(639, 16).put(479, 16).flag(false);
        parse_header(&w.bytes())
    }

    #[test]
    fn profile_1_444_and_rgb() {
        let h = key(1, |w| {
            w.put(2, 3).flag(true).flag(false).flag(false).flag(false);
        })
        .unwrap();
        assert_eq!(
            (h.subsampling, h.full_range, h.bit_depth),
            (Some((0, 0)), Some(true), Some(8))
        );
        assert_eq!(
            synthesize_vpcc(&h, 30.0, &ColourTags::default()).unwrap()[6],
            (8 << 4) | (3 << 1) | 1
        );
        let rgb = key(1, |w| {
            w.put(7, 3).flag(false);
        })
        .unwrap();
        assert_eq!(
            (rgb.color_space, rgb.subsampling),
            (Some(CS_RGB), Some((0, 0)))
        );
        assert!(
            key(0, |w| {
                w.put(7, 3);
            })
            .is_err(),
            "RGB in profile 0"
        );
        assert!(
            key(1, |w| {
                w.put(2, 3).flag(false).flag(true).flag(true).flag(false);
            })
            .is_err(),
            "4:2:0 in profile 1"
        );
        assert!(
            key(1, |w| {
                w.put(2, 3).flag(false).flag(true).flag(false).flag(true);
            })
            .is_err(),
            "reserved bit set"
        );
    }

    #[test]
    fn profile_2_ten_and_twelve_bit() {
        let ten = key(2, |w| {
            w.flag(false).put(2, 3).flag(false);
        })
        .unwrap();
        assert_eq!(
            (ten.profile, ten.bit_depth, ten.subsampling),
            (2, Some(10), Some((1, 1)))
        );
        let twelve = key(3, |w| {
            w.flag(true)
                .put(1, 3)
                .flag(false)
                .flag(true)
                .flag(false)
                .flag(false);
        })
        .unwrap();
        assert_eq!(
            (twelve.bit_depth, twelve.subsampling),
            (Some(12), Some((1, 0)))
        );
    }

    #[test]
    fn intra_only_profile_0_and_show_existing() {
        let mut w = BitWriter::default();
        // marker, profile 0, show_existing 0, frame_type 1 (non-key), show 0, error_res 0
        w.put(2, 2)
            .put(0, 2)
            .flag(false)
            .flag(true)
            .flag(false)
            .flag(false);
        w.flag(true)
            .put(0, 2)
            .put(0x49_8342, 24)
            .put(0xFF, 8)
            .put(319, 16)
            .put(239, 16)
            .flag(false);
        let h = parse_header(&w.bytes()).unwrap();
        assert!(h.intra_only && !h.key_frame && !h.show_frame);
        assert_eq!(
            (h.width, h.bit_depth, h.color_space),
            (Some(320), Some(8), Some(1))
        );
        let mut w = BitWriter::default();
        w.put(2, 2).put(0, 2).flag(true).put(5, 3);
        let e = parse_header(&w.bytes()).unwrap();
        assert!(e.show_existing_frame && e.show_frame);
        assert_eq!((e.frame_to_show, e.width), (Some(5), None));
    }

    #[test]
    fn superframe_padding_and_empty_entries() {
        // frames 3 and 2 bytes, one padding byte, a zero-size entry, 1-byte sizes, 3 frames
        let p = [1, 2, 3, 4, 5, 0xEE, 0xC2, 3, 2, 0, 0xC2];
        assert_eq!(superframe(&p).unwrap(), vec![(0, 3), (3, 2)]);
    }
}
