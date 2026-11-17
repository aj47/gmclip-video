//! `stsd` sample entries: the codec, its configuration record and the picture or sound
//! description. Configuration records are carried as bytes; `codec::` reads them.

use super::super::io::Cursor;
use super::super::{Codec, DemuxError, Result, TrackKind};
use super::boxes::{self, Node, fourcc};

/// What the first sample entry says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Bytes per PCM frame (all channels) for uncompressed audio.
    pub pcm_frame_bytes: Option<u32>,
    pub colour: crate::codec::ColourTags,
}

/// Parse `stsd`; returns the first entry and the entry count.
/// `quicktime` selects the QuickTime sound description layout (version 1/2 extension
/// fields) over the ISO one (ISO `AudioSampleEntryV1` has no extension fields).
pub fn parse_stsd(stsd: &Node<'_>, kind: TrackKind, quicktime: bool) -> Result<(Entry, u32)> {
    let mut c = stsd.cursor();
    c.skip(4)?; // version + flags
    let count = c.u32()?;
    if count == 0 {
        return Err(DemuxError::invalid(
            "stsd has no sample entries",
            stsd.offset,
        ));
    }
    let at = c.offset();
    let (fmt, head, size) = boxes::header(&mut c)?;
    let body = usize::try_from(size.saturating_sub(head as u64))
        .map_err(|_| DemuxError::invalid("huge sample entry", at))?;
    let mut e = c.sub(body)?;
    let entry = match kind {
        TrackKind::Video => video(fmt, &mut e)?,
        TrackKind::Audio => audio(fmt, &mut e, quicktime)?,
        TrackKind::Other => Entry {
            codec: Codec::Other { name: fourcc(&fmt) },
            width: 0,
            height: 0,
            sample_rate: 0,
            channels: 0,
            pcm_frame_bytes: None,
            colour: Default::default(),
        },
    };
    Ok((entry, count))
}

/// The child boxes after a sample entry's fixed fields (bounded recursion: only `wave`
/// is opened, one level).
fn children<'a>(c: &mut Cursor<'a>) -> Result<Vec<Node<'a>>> {
    let base = c.offset();
    Ok(boxes::flat(c.rest(), base))
}

fn video(fmt: [u8; 4], c: &mut Cursor<'_>) -> Result<Entry> {
    c.skip(8)?; // reserved + data_reference_index
    c.skip(16)?; // pre_defined, reserved, pre_defined[3]
    let width = u32::from(c.u16()?);
    let height = u32::from(c.u16()?);
    c.skip(50)?; // resolutions, reserved, frame_count, compressorname, depth, pre_defined
    let kids = children(c)?;
    let record = |k: &[u8; 4]| {
        kids.iter()
            .find(|n| &n.kind == k)
            .map(|n| n.payload.to_vec())
    };
    let codec = match &fmt {
        b"avc1" | b"avc3" => Codec::H264 {
            avcc: record(b"avcC")
                .ok_or_else(|| DemuxError::invalid("avc1 without avcC", c.offset()))?,
        },
        b"hvc1" | b"hev1" => Codec::Hevc {
            hvcc: record(b"hvcC").unwrap_or_default(),
        },
        b"vp09" => Codec::Vp9 {
            vpcc: record(b"vpcC"),
        },
        b"av01" => Codec::Av1 {
            av1c: record(b"av1C").unwrap_or_default(),
        },
        other => Codec::Other {
            name: fourcc(other),
        },
    };
    let colour = kids
        .iter()
        .find(|n| &n.kind == b"colr")
        .map(colr)
        .unwrap_or_default();
    Ok(Entry {
        codec,
        width,
        height,
        sample_rate: 0,
        channels: 0,
        pcm_frame_bytes: None,
        colour,
    })
}

fn audio(fmt: [u8; 4], c: &mut Cursor<'_>, quicktime: bool) -> Result<Entry> {
    c.skip(8)?; // reserved + data_reference_index
    let version = c.u16()?;
    c.skip(6)?; // revision, vendor
    let mut channels = c.u16()?;
    let mut bits = c.u16()?;
    c.skip(4)?; // compression_id, packet_size
    let mut sample_rate = c.u32()? >> 16;
    let mut lpcm_flags = 0u32;
    match version {
        0 => {}
        _ if !quicktime => {} // ISO AudioSampleEntryV1: children follow directly
        1 => c.skip(16)?,     // samples/packet, bytes/packet, bytes/frame, bytes/sample
        2 => {
            c.skip(4)?; // sizeOfStructOnly
            let rate = f64::from_bits(c.u64()?);
            channels = u16::try_from(c.u32()?)
                .map_err(|_| DemuxError::invalid("channel count", c.offset()))?;
            c.skip(4)?; // always 0x7F000000
            bits = u16::try_from(c.u32()?)
                .map_err(|_| DemuxError::invalid("bits per channel", c.offset()))?;
            lpcm_flags = c.u32()?;
            c.skip(8)?; // constBytesPerAudioPacket, constLPCMFramesPerAudioPacket
            if rate.is_finite() && rate > 0.0 && rate < 1.0e7 {
                sample_rate = rate as u32;
            }
        }
        v => {
            return Err(DemuxError::unsupported(format!(
                "sound sample description version {v}"
            )));
        }
    }
    let kids = children(c)?;
    // QuickTime nests esds/enda inside `wave`.
    let wave: Vec<Node<'_>> = kids
        .iter()
        .find(|n| &n.kind == b"wave")
        .map(|w| boxes::flat(w.payload, w.payload_offset))
        .unwrap_or_default();
    let find = |k: &[u8; 4]| kids.iter().chain(wave.iter()).find(|n| &n.kind == k);
    // ISO `srat`: the real rate when it does not fit the 16.16 field.
    if let Some(srat) = find(b"srat") {
        let mut sc = srat.cursor();
        sc.skip(4)?;
        sample_rate = sc.u32()?;
    }
    let little = find(b"enda")
        .and_then(|n| n.payload.get(..2))
        .is_some_and(|b| b != [0, 0]);
    let pcm = |bits: u16, float: bool, little_endian: bool| Codec::Pcm {
        bits,
        float,
        little_endian,
    };
    let codec = match &fmt {
        b"mp4a" => {
            let esds = find(b"esds")
                .ok_or_else(|| DemuxError::invalid("mp4a without esds", c.offset()))?;
            let (oti, asc) = parse_esds(esds)?;
            match oti {
                0x40 | 0x66..=0x68 => Codec::Aac { asc },
                0x69 | 0x6B => Codec::Other { name: "mp3".into() },
                o => Codec::Other {
                    name: format!("mp4a/{o:#04x}"),
                },
            }
        }
        b"ipcm" | b"fpcm" => {
            let p = find(b"pcmC")
                .ok_or_else(|| DemuxError::invalid("ipcm without pcmC", c.offset()))?;
            let mut pc = p.cursor();
            pc.skip(4)?;
            let flags = pc.u8()?;
            let size = u16::from(pc.u8()?);
            pcm(size, &fmt == b"fpcm", flags & 1 == 1)
        }
        b"in24" => pcm(24, false, little),
        b"in32" => pcm(32, false, little),
        b"fl32" => pcm(32, true, little),
        b"fl64" => pcm(64, true, little),
        b"sowt" => pcm(bits.max(16), false, true),
        b"twos" => pcm(bits, false, false),
        b"raw " => pcm(8, false, false),
        b"lpcm" => pcm(bits, lpcm_flags & 1 == 1, lpcm_flags & 2 == 0),
        b"Opus" => Codec::Opus {
            config: find(b"dOps")
                .map(|n| n.payload.to_vec())
                .unwrap_or_default(),
        },
        other => Codec::Other {
            name: fourcc(other),
        },
    };
    let pcm_frame_bytes = match &codec {
        Codec::Pcm { bits, .. } if *bits % 8 == 0 && *bits > 0 && channels > 0 => {
            Some(u32::from(*bits / 8) * u32::from(channels))
        }
        Codec::Pcm { .. } => {
            return Err(DemuxError::unsupported(format!(
                "PCM with {bits} bits, {channels} channels"
            )));
        }
        _ => None,
    };
    Ok(Entry {
        codec,
        width: 0,
        height: 0,
        sample_rate,
        channels,
        pcm_frame_bytes,
        colour: Default::default(),
    })
}

/// `colr` with `nclx` (ISO) or `nclc` (QuickTime): primaries, transfer, matrix and,
/// for nclx, the full-range flag. Other colour types (ICC profiles) carry no tags.
fn colr(n: &Node<'_>) -> crate::codec::ColourTags {
    let mut c = n.cursor();
    let parse = |c: &mut Cursor<'_>| -> Result<crate::codec::ColourTags> {
        let kind = c.fourcc()?;
        if &kind != b"nclx" && &kind != b"nclc" {
            return Ok(Default::default());
        }
        let small = |v: u16| u8::try_from(v).ok();
        let primaries = small(c.u16()?);
        let transfer = small(c.u16()?);
        let matrix = small(c.u16()?);
        let full_range = if &kind == b"nclx" {
            Some(c.u8()? & 0x80 != 0)
        } else {
            None
        };
        Ok(crate::codec::ColourTags {
            primaries,
            transfer,
            matrix,
            full_range,
            chroma_left: None,
        })
    };
    parse(&mut c).unwrap_or_default()
}

/// MPEG-4 descriptor length: 1–4 bytes of 7 bits.
fn desc_len(c: &mut Cursor<'_>) -> Result<usize> {
    let mut n = 0usize;
    for _ in 0..4 {
        let b = c.u8()?;
        n = (n << 7) | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            return Ok(n);
        }
    }
    Ok(n)
}

/// `esds`: the objectTypeIndication and the DecoderSpecificInfo (AudioSpecificConfig).
pub fn parse_esds(esds: &Node<'_>) -> Result<(u8, Vec<u8>)> {
    let mut c = esds.cursor();
    c.skip(4)?;
    if c.u8()? != 0x03 {
        return Err(DemuxError::invalid(
            "esds without ES_Descriptor",
            esds.offset,
        ));
    }
    let n = desc_len(&mut c)?;
    let mut es = c.sub(n.min(c.remaining()))?;
    es.skip(2)?; // ES_ID
    let flags = es.u8()?;
    if flags & 0x80 != 0 {
        es.skip(2)?;
    }
    if flags & 0x40 != 0 {
        let url = usize::from(es.u8()?);
        es.skip(url)?;
    }
    if flags & 0x20 != 0 {
        es.skip(2)?;
    }
    if es.u8()? != 0x04 {
        return Err(DemuxError::invalid(
            "esds without DecoderConfigDescriptor",
            esds.offset,
        ));
    }
    let n = desc_len(&mut es)?;
    let mut dc = es.sub(n.min(es.remaining()))?;
    let oti = dc.u8()?;
    dc.skip(12)?; // streamType, bufferSize, maxBitrate, avgBitrate
    let mut asc = Vec::new();
    if !dc.is_empty() && dc.u8()? == 0x05 {
        let n = desc_len(&mut dc)?;
        asc = dc.bytes(n.min(dc.remaining()))?.to_vec();
    }
    Ok((oti, asc))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn esds_yields_the_audio_specific_config() {
        // ES_Descriptor > DecoderConfigDescriptor(0x40) > DecSpecificInfo [0x11, 0x90]
        let payload = [
            0, 0, 0, 0, 0x03, 0x19, 0x00, 0x01, 0x00, 0x04, 0x11, 0x40, 0x15, 0, 0, 0, 0, 1, 0xF4,
            0, 0, 1, 0xF4, 0, 0x05, 0x02, 0x11, 0x90, 0x06, 0x01, 0x02,
        ];
        let node = Node {
            kind: *b"esds",
            offset: 0,
            payload: &payload,
            payload_offset: 8,
            children: vec![],
        };
        let (oti, asc) = parse_esds(&node).unwrap();
        assert_eq!(oti, 0x40);
        assert_eq!(asc, vec![0x11, 0x90]);
    }

    #[test]
    fn truncated_esds_is_invalid_not_a_panic() {
        for n in 0..12 {
            let payload = [0u8, 0, 0, 0, 0x03, 0x80, 0x80, 0x80, 0x7F, 0, 1, 0][..n].to_vec();
            let node = Node {
                kind: *b"esds",
                offset: 0,
                payload: &payload,
                payload_offset: 8,
                children: vec![],
            };
            assert!(parse_esds(&node).is_err());
        }
    }
}
