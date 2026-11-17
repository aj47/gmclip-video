//! RIFF/WAVE (decision 0051: music and sound effects come as WAV): one PCM track whose
//! index is packets of [`super::mp4::stbl::PCM_PACKET_FRAMES`] frames of the `data`
//! chunk, as the MP4 demuxer indexes uncompressed audio. `fmt ` tags 1 (integer PCM),
//! 3 (IEEE float) and `WAVE_FORMAT_EXTENSIBLE` with either subformat; anything else is
//! `Unsupported`. A `data` size past the end of the file (a stream that was never
//! finalised) is cut to the file.

use super::io::{ByteSource, vec_with_capacity};
use super::mp4::stbl::PCM_PACKET_FRAMES;
use super::{
    Codec, Container, DemuxError, Demuxed, MAX_SAMPLES, Result, Track, TrackKind, entry_budget,
    finish_index,
};

/// Whether the first 12 bytes are a RIFF/WAVE header.
pub fn is_wav(head: &[u8]) -> bool {
    head.get(..4) == Some(b"RIFF".as_slice()) && head.get(8..12) == Some(b"WAVE".as_slice())
}

fn le16(b: &[u8], at: usize) -> u16 {
    let mut w = [0u8; 2];
    if let Some(s) = b.get(at..at + 2) {
        w.copy_from_slice(s);
    }
    u16::from_le_bytes(w)
}

fn le32(b: &[u8], at: usize) -> u32 {
    let mut w = [0u8; 4];
    if let Some(s) = b.get(at..at + 4) {
        w.copy_from_slice(s);
    }
    u32::from_le_bytes(w)
}

/// Indexes a WAV file.
pub fn index<S: ByteSource>(src: &mut S) -> Result<Demuxed> {
    let len = src.len();
    let mut at = 12u64;
    let mut fmt: Option<(u16, u16, u32, u16, u16)> = None;
    let mut data: Option<(u64, u64)> = None;
    let mut chunks = 0u32;
    while at + 8 <= len && data.is_none() {
        chunks += 1;
        if chunks > 4096 {
            return Err(DemuxError::budget("RIFF chunks", at));
        }
        let mut h = [0u8; 8];
        src.read_at(at, &mut h)?;
        let size = u64::from(le32(&h, 4));
        let body = at + 8;
        match h.get(..4) {
            Some(b"fmt ") => {
                if !(16..=1024).contains(&size) || body + size > len {
                    return Err(DemuxError::invalid("WAV fmt chunk", at));
                }
                let mut f = vec![0u8; size as usize];
                src.read_at(body, &mut f)?;
                let mut tag = le16(&f, 0);
                if tag == 0xFFFE && size >= 40 {
                    tag = le16(&f, 24); // the subformat GUID's first two bytes
                }
                fmt = Some((tag, le16(&f, 2), le32(&f, 4), le16(&f, 12), le16(&f, 14)));
            }
            Some(b"data") => data = Some((body, size.min(len - body))),
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    let (tag, channels, rate, align, bits) =
        fmt.ok_or_else(|| DemuxError::invalid("WAV without a fmt chunk", 12))?;
    let (off, size) = data.ok_or_else(|| DemuxError::invalid("WAV without a data chunk", 12))?;
    let float = match tag {
        1 => false,
        3 => true,
        t => return Err(DemuxError::unsupported(format!("WAV format tag {t}"))),
    };
    if channels == 0 || rate == 0 || bits == 0 || bits % 8 != 0 {
        return Err(DemuxError::invalid("WAV format", 12));
    }
    let frame = u64::from(channels) * u64::from(bits / 8);
    if u64::from(align) != frame {
        return Err(DemuxError::invalid("WAV block align", 12));
    }
    let frames = size / frame;
    let packets = frames.div_ceil(u64::from(PCM_PACKET_FRAMES));
    if packets > MAX_SAMPLES || packets > entry_budget(len) {
        return Err(DemuxError::budget("WAV packets", off));
    }
    let n = packets as usize;
    let (mut dts, mut cts, mut offs, mut sizes, mut sync, mut durs) = (
        vec_with_capacity(n, off)?,
        vec_with_capacity(n, off)?,
        vec_with_capacity(n, off)?,
        vec_with_capacity(n, off)?,
        vec_with_capacity(n, off)?,
        vec_with_capacity::<u32>(n, off)?,
    );
    let mut t = 0u64;
    while t < frames {
        let k = (frames - t).min(u64::from(PCM_PACKET_FRAMES));
        dts.push(t as i64);
        cts.push(0);
        offs.push(off + t * frame);
        sizes.push((k * frame) as u32);
        sync.push(true);
        durs.push(k as u32);
        t += k;
    }
    let samples = finish_index(rate, TrackKind::Audio, dts, cts, offs, sizes, sync, &durs)?;
    Ok(Demuxed {
        container: Container::Wav,
        tracks: vec![Track {
            id: 1,
            kind: TrackKind::Audio,
            codec: Codec::Pcm {
                bits,
                float,
                little_endian: true,
            },
            timescale: rate,
            width: 0,
            height: 0,
            sample_rate: rate,
            channels,
            colour: Default::default(),
            samples,
            durations: durs,
            pts_unknown: Vec::new(),
            block_pos: Vec::new(),
            edits: Vec::new(),
            movie_timescale: rate,
        }],
        cues: Vec::new(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::super::io::SliceSource;
    use super::*;

    fn wav(tag: u16, ch: u16, rate: u32, bits: u16, frames: u32, extra: &[u8]) -> Vec<u8> {
        let align = ch * bits / 8;
        let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
        v.extend_from_slice(b"LIST");
        v.extend_from_slice(&(extra.len() as u32).to_le_bytes());
        v.extend_from_slice(extra);
        if extra.len() % 2 == 1 {
            v.push(0);
        }
        v.extend_from_slice(b"fmt \x10\0\0\0");
        for x in [tag, ch] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        v.extend_from_slice(&align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(frames * u32::from(align)).to_le_bytes());
        v.extend(std::iter::repeat_n(
            7u8,
            (frames * u32::from(align)) as usize,
        ));
        v
    }

    #[test]
    fn a_wav_is_one_pcm_track_in_packets() {
        let b = wav(1, 2, 44_100, 24, 2500, b"odd");
        assert!(is_wav(&b[..12]));
        let d = index(&mut SliceSource(&b)).unwrap();
        assert_eq!(d.container, Container::Wav);
        let t = &d.tracks[0];
        assert_eq!(
            t.codec,
            Codec::Pcm {
                bits: 24,
                float: false,
                little_endian: true
            }
        );
        assert_eq!(
            (t.sample_rate, t.channels, t.timescale),
            (44_100, 2, 44_100)
        );
        assert_eq!(t.samples.dts, [0, 1024, 2048]);
        assert_eq!(t.samples.size, [1024 * 6, 1024 * 6, 452 * 6]);
        assert_eq!(t.samples.offset[1] - t.samples.offset[0], 1024 * 6);
        assert_eq!(t.audio_start(), Some((0, 0)));
        let f = index(&mut SliceSource(&wav(3, 1, 48_000, 32, 10, b""))).unwrap();
        assert!(matches!(f.tracks[0].codec, Codec::Pcm { float: true, .. }));
    }

    #[test]
    fn what_a_wav_cannot_say_is_refused() {
        assert!(matches!(
            index(&mut SliceSource(&wav(2, 2, 44_100, 16, 10, b""))),
            Err(DemuxError::Unsupported { .. })
        ));
        let mut cut = wav(1, 2, 48_000, 16, 100, b"");
        cut.truncate(cut.len() - 40); // a data size past the end: cut to the file
        let d = index(&mut SliceSource(&cut)).unwrap();
        assert_eq!(d.tracks[0].durations, [90]);
        assert!(index(&mut SliceSource(b"RIFF\0\0\0\0WAVEdata\0\0\0\0")).is_err());
    }
}
