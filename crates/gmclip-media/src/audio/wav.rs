//! Minimal RIFF/WAVE PCM reader and f32 writer. Used to hand decoded PCM between
//! gmclip and dev tools (`tools/` decodes references to WAV); not a codec: the payload
//! is already PCM. Every length read from a file is checked.

use std::io::Write;

/// Interleaved PCM as f32.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    /// Interleaved samples, `frames × channels`.
    pub samples: Vec<f32>,
}

impl Pcm {
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WavError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a WAV file: {0}")]
    Format(&'static str),
    #[error("unsupported WAV encoding: format {format}, {bits} bits")]
    Unsupported { format: u16, bits: u16 },
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at.checked_add(2)?)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at.checked_add(4)?)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parses a WAV file: PCM 16/24/32-bit integer or 32/64-bit float, plain or
/// WAVE_FORMAT_EXTENSIBLE. Integer samples map to f32 by dividing by 2^(bits−1).
#[allow(clippy::indexing_slicing)] // fixed-size slices from checked `get`
pub fn parse(d: &[u8]) -> Result<Pcm, WavError> {
    if d.get(0..4) != Some(b"RIFF") || d.get(8..12) != Some(b"WAVE") {
        return Err(WavError::Format("no RIFF/WAVE header"));
    }
    let mut at = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while let (Some(id), Some(len)) = (d.get(at..at + 4), u32le(d, at + 4)) {
        let body = at + 8;
        let end = body
            .checked_add(len as usize)
            .ok_or(WavError::Format("chunk length"))?;
        match id {
            b"fmt " => {
                let mut format = u16le(d, body).ok_or(WavError::Format("short fmt"))?;
                let channels = u16le(d, body + 2).ok_or(WavError::Format("short fmt"))?;
                let rate = u32le(d, body + 4).ok_or(WavError::Format("short fmt"))?;
                let bits = u16le(d, body + 14).ok_or(WavError::Format("short fmt"))?;
                if format == 0xfffe {
                    format = u16le(d, body + 24).ok_or(WavError::Format("short extensible fmt"))?;
                }
                if channels == 0 {
                    return Err(WavError::Format("0 channels"));
                }
                fmt = Some((format, channels, rate, bits));
            }
            b"data" => {
                let (format, channels, rate, bits) =
                    fmt.ok_or(WavError::Format("data before fmt"))?;
                // ffmpeg writes 0xFFFFFFFF lengths when piping; take what is there.
                let data = d
                    .get(body..end.min(d.len()))
                    .ok_or(WavError::Format("data chunk"))?;
                let samples: Vec<f32> = match (format, bits) {
                    (1, 16) => data
                        .chunks_exact(2)
                        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0)
                        .collect(),
                    (1, 24) => data
                        .chunks_exact(3)
                        .map(|b| {
                            (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0
                        })
                        .collect(),
                    (1, 32) => data
                        .chunks_exact(4)
                        .map(|b| {
                            (f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                / 2_147_483_648.0) as f32
                        })
                        .collect(),
                    (3, 32) => data
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect(),
                    #[allow(clippy::cast_possible_truncation)]
                    (3, 64) => data
                        .chunks_exact(8)
                        .map(|b| {
                            f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
                                as f32
                        })
                        .collect(),
                    _ => return Err(WavError::Unsupported { format, bits }),
                };
                let frames = samples.len() / usize::from(channels);
                let mut samples = samples;
                samples.truncate(frames * usize::from(channels));
                return Ok(Pcm {
                    rate,
                    channels,
                    samples,
                });
            }
            _ => {}
        }
        // Chunks are word-aligned.
        at = end
            .checked_add(len as usize & 1)
            .ok_or(WavError::Format("chunk length"))?;
    }
    Err(WavError::Format("no data chunk"))
}

/// Reads a WAV file from disk.
pub fn read(path: &std::path::Path) -> Result<Pcm, WavError> {
    parse(&std::fs::read(path)?)
}

/// Writes interleaved f32 PCM as a 32-bit float WAV.
pub fn write_f32<W: Write>(
    mut w: W,
    rate: u32,
    channels: u16,
    samples: &[f32],
) -> Result<(), WavError> {
    let data_len =
        u32::try_from(samples.len() * 4).map_err(|_| WavError::Format("data larger than 4 GiB"))?;
    let block = channels
        .checked_mul(4)
        .ok_or(WavError::Format("channels"))?;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?;
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * u32::from(block)).to_le_bytes())?;
    w.write_all(&block.to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    let mut buf = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    w.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn f32_round_trip() {
        let s: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.01).sin()).collect();
        let mut b = Vec::new();
        write_f32(&mut b, 48_000, 2, &s).unwrap();
        let p = parse(&b).unwrap();
        assert_eq!((p.rate, p.channels, p.frames()), (48_000, 2, 500));
        assert_eq!(p.samples, s);
    }

    #[test]
    fn int_formats_scale() {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&[1, 0, 1, 0]);
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&(48_000u32 * 3).to_le_bytes());
        b.extend_from_slice(&[3, 0, 24, 0]);
        b.extend_from_slice(b"data");
        b.extend_from_slice(&6u32.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0x40, 0, 0, 0xc0]); // +0.5, −0.5
        let p = parse(&b).unwrap();
        assert_eq!(p.samples, vec![0.5, -0.5]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"RIFF").is_err());
        assert!(parse(b"RIFF\0\0\0\0WAVEdata\xff\xff\xff\xff").is_err());
    }
}
