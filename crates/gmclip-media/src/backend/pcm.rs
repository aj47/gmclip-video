//! Random access to a track of linear PCM (MOV `lpcm`/`sowt`/`twos`/`in24`/`fl32`…, a
//! WAV file's data; decision 0051) by frame index, as interleaved f32: what the mix
//! graph's `PcmSource` needs, as [`super::aac::AacRandomReader`] gives it for AAC.
//! Uncompressed samples need no decoder: integers are scaled by `2^(bits−1)` and floats
//! read as they are, so every read of a frame gives the same value.

use crate::backend::source::SampleSource;
use crate::decode::MediaError;
use crate::index::SampleIdx;

/// How a track's PCM is stored.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PcmFormat {
    /// 16, 24 or 32 for integers (signed), 32 or 64 for floats.
    pub bits: u16,
    pub float: bool,
    pub little_endian: bool,
}

impl PcmFormat {
    /// Whether this build reads it (8-bit PCM's signedness differs by container).
    #[must_use]
    pub fn readable(self) -> bool {
        if self.float {
            matches!(self.bits, 32 | 64)
        } else {
            matches!(self.bits, 16 | 24 | 32)
        }
    }

    fn bytes(self) -> usize {
        usize::from(self.bits / 8)
    }

    /// One stored sample as f32.
    fn value(self, b: &[u8]) -> f32 {
        let mut w = [0u8; 8];
        let n = self.bytes().min(b.len()).min(8);
        // little-endian into the low bytes of `w`
        for (k, x) in w.iter_mut().enumerate().take(n) {
            *x = if self.little_endian {
                b[k]
            } else {
                b[n - 1 - k]
            };
        }
        let u = u64::from_le_bytes(w);
        match (self.float, self.bits) {
            (true, 32) => f32::from_bits(u as u32),
            (true, _) => f64::from_bits(u) as f32,
            (false, bits) => {
                let shift = 64 - u32::from(bits);
                let v = ((u << shift) as i64) >> shift;
                (v as f64 / (1u64 << (bits - 1)) as f64) as f32
            }
        }
    }
}

/// Random access by frame over a PCM track whose index entries are packets of frames
/// (the demuxers' PCM packets; `dts` counts frames).
#[derive(Debug)]
pub struct PcmRandomReader<S: SampleSource> {
    source: S,
    fmt: PcmFormat,
    channels: usize,
    skip: u64,
    lead: u64,
    /// The packet held decoded, and its samples.
    held: Option<usize>,
    pcm: Vec<f32>,
    buf: Vec<u8>,
}

impl<S: SampleSource> PcmRandomReader<S> {
    pub fn new(source: S, fmt: PcmFormat, channels: u32, skip: u64) -> Self {
        Self {
            source,
            fmt,
            channels: channels.max(1) as usize,
            skip,
            lead: 0,
            held: None,
            pcm: Vec::new(),
            buf: Vec::new(),
        }
    }

    /// `lead` frames of silence before the first one kept (an empty edit in front).
    #[must_use]
    pub fn with_lead(mut self, lead: u64) -> Self {
        self.lead = lead;
        self
    }

    fn load(&mut self, k: usize) -> Result<(), MediaError> {
        if self.held == Some(k) {
            return Ok(());
        }
        self.source.read(SampleIdx(k as u32), &mut self.buf)?;
        let w = self.fmt.bytes();
        self.pcm.clear();
        self.pcm
            .extend(self.buf.chunks_exact(w).map(|b| self.fmt.value(b)));
        self.held = Some(k);
        Ok(())
    }

    /// Fills `out` (interleaved) from frame `first` (after the edit list's skip, counting
    /// the lead) on; frames past the end are silence.
    pub fn read(&mut self, first: u64, out: &mut [f32]) -> Result<(), MediaError> {
        let ch = self.channels;
        out.fill(0.0);
        let frames = (out.len() / ch) as u64;
        let mut m = 0u64; // frames of `out` done
        while m < frames {
            let at = first + m;
            if at < self.lead {
                m += (self.lead - at).min(frames - m);
                continue;
            }
            let pos = i64::try_from(at - self.lead + self.skip).unwrap_or(i64::MAX);
            let dts = &self.source.index().dts;
            let k = dts.partition_point(|&d| d <= pos);
            if k == 0 {
                break;
            }
            let k = k - 1;
            let start = dts[k];
            self.load(k)?;
            let have = (self.pcm.len() / ch) as i64;
            let off = pos - start;
            if off >= have {
                break; // past the last packet
            }
            let n = ((have - off) as u64).min(frames - m) as usize;
            let (a, d) = (off as usize * ch, m as usize * ch);
            out[d..d + n * ch].copy_from_slice(&self.pcm[a..a + n * ch]);
            m += n as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::index::SampleIndex;

    /// 16-bit little-endian stereo in packets of 3 frames: frame `i` is `(i, −i)`.
    struct Packets {
        index: SampleIndex,
        data: Vec<Vec<u8>>,
    }

    impl SampleSource for Packets {
        fn index(&self) -> &SampleIndex {
            &self.index
        }
        fn read(&mut self, s: SampleIdx, buf: &mut Vec<u8>) -> Result<(), MediaError> {
            buf.clone_from(&self.data[s.0 as usize]);
            Ok(())
        }
    }

    fn packets(frames: i64) -> Packets {
        let mut index = SampleIndex::default();
        let mut data = Vec::new();
        let mut t = 0;
        while t < frames {
            let n = (frames - t).min(3);
            index.dts.push(t);
            data.push(
                (t..t + n)
                    .flat_map(|i| {
                        let (l, r) = (i as i16 * 100, -(i as i16) * 100);
                        [l.to_le_bytes(), r.to_le_bytes()].concat()
                    })
                    .collect(),
            );
            t += n;
        }
        Packets { index, data }
    }

    #[test]
    fn frames_read_across_packets_with_skip_lead_and_end() {
        let fmt = PcmFormat {
            bits: 16,
            float: false,
            little_endian: true,
        };
        let mut r = PcmRandomReader::new(packets(10), fmt, 2, 1).with_lead(2);
        let mut out = vec![9.0f32; 2 * 12];
        r.read(0, &mut out).unwrap();
        let v = |i: i16| f32::from(i * 100) / 32768.0;
        // two frames of lead, then frames 1..10 (one skipped), then silence
        assert_eq!(&out[..4], &[0.0; 4]);
        assert_eq!((out[4], out[5]), (v(1), -v(1)));
        assert_eq!(out[2 * 10], v(9));
        assert_eq!(&out[2 * 11..], &[0.0; 2]);
        // a read in the middle of a packet, backwards
        let mut one = [0.0f32; 2];
        r.read(6, &mut one).unwrap();
        assert_eq!(one, [v(5), -v(5)]);
    }

    #[test]
    fn integer_and_float_layouts() {
        let f = |bits, float, le| PcmFormat {
            bits,
            float,
            little_endian: le,
        };
        assert_eq!(f(24, false, true).value(&[0, 0, 0x80]), -1.0);
        assert_eq!(f(24, false, false).value(&[0x40, 0, 0]), 0.5);
        assert_eq!(f(16, false, false).value(&[0xff, 0xff]), -1.0 / 32768.0);
        assert_eq!(f(32, true, true).value(&0.25f32.to_le_bytes()), 0.25);
        assert_eq!(f(64, true, false).value(&(-0.5f64).to_be_bytes()), -0.5);
        assert!(!f(8, false, true).readable() && f(24, false, true).readable());
    }
}
