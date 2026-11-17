//! The seam between a demuxer and a decoder: a sample index plus a way to read one
//! sample's bytes. Lane B's MP4/MKV demuxers implement it (B-01, B-02); lab tests use
//! a temporary ffprobe-driven shim until those land.

use crate::decode::MediaError;
use crate::index::{SampleIdx, SampleIndex};

/// A video or audio track that can hand out coded samples by index.
pub trait SampleSource {
    /// The track's sample index (decode order, with `pts_order`).
    fn index(&self) -> &SampleIndex;

    /// Reads sample `s` into `buf` (replacing its contents).
    fn read(&mut self, s: SampleIdx, buf: &mut Vec<u8>) -> Result<(), MediaError>;

    /// Presentation timestamp of sample `s` in the index timescale.
    fn pts(&self, s: SampleIdx) -> i64 {
        let i = self.index();
        let k = s.0 as usize;
        i.dts[k] + i64::from(i.cts_off[k])
    }
}

impl<T: SampleSource + ?Sized> SampleSource for Box<T> {
    fn index(&self) -> &SampleIndex {
        (**self).index()
    }

    fn read(&mut self, s: SampleIdx, buf: &mut Vec<u8>) -> Result<(), MediaError> {
        (**self).read(s, buf)
    }
}
