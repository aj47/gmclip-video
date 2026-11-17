//! The sample index (implementer Part C): struct-of-arrays in decode order.

use gmclip_model::Rational;

/// Index of a sample in decode order.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug)]
pub struct SampleIdx(pub u32);

/// One track's samples. `sync` becomes a bit vector when B-01 lands.
#[derive(Clone, Debug, Default)]
pub struct SampleIndex {
    pub timescale: u32,
    pub dts: Vec<i64>,
    pub cts_off: Vec<i32>,
    pub offset: Vec<u64>,
    pub size: Vec<u32>,
    pub sync: Vec<bool>,
    /// Sample indices sorted by PTS (presentation order).
    pub pts_order: Vec<u32>,
    pub reorder_depth: u8,
    pub cfr: Option<Rational>,
    /// False while an MKV bisection or scan is still running.
    pub complete: bool,
}

impl SampleIndex {
    pub fn len(&self) -> usize {
        self.dts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dts.is_empty()
    }
}
