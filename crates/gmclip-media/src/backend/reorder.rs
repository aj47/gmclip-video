//! The PTS reorder stage (SPEC §6.2): decoders hand frames back in decode order (or
//! any order); this puts them in presentation order.
//!
//! Two modes:
//! - **expected** (the default): the caller announces the PTS of every frame it will
//!   ask to see with [`ReorderStage::expect`] **before** pushing any decoded frame that
//!   could follow it in presentation order (in practice: announce a whole range, or at
//!   least the next reorder window, ahead of submitting). A frame is released as soon
//!   as it is the earliest still-expected PTS, so no frame waits longer than its own
//!   decode, and nothing depends on a reorder depth read from the stream.
//! - **depth**: for streams without an index; a frame is released once more than
//!   `depth` frames are buffered (the SPS `max_num_reorder_frames` rule).

use std::collections::BTreeMap;

/// A presentation-order buffer of decoded frames.
#[derive(Debug)]
pub struct ReorderStage<T> {
    mode: Mode,
    /// Expected PTS → how many frames with that PTS are still to come.
    expected: BTreeMap<i64, u32>,
    /// Frames that arrived, keyed by (PTS, arrival sequence).
    ready: BTreeMap<(i64, u64), T>,
    seq: u64,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Mode {
    Expected,
    Depth(usize),
}

impl<T> Default for ReorderStage<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ReorderStage<T> {
    /// Expected-PTS mode.
    pub fn new() -> Self {
        Self {
            mode: Mode::Expected,
            expected: BTreeMap::new(),
            ready: BTreeMap::new(),
            seq: 0,
        }
    }

    /// Depth mode: release when more than `depth` frames are buffered.
    pub fn with_depth(depth: usize) -> Self {
        Self {
            mode: Mode::Depth(depth),
            ..Self::new()
        }
    }

    /// Announces that a frame with `pts` will be pushed (expected mode).
    pub fn expect(&mut self, pts: i64) {
        *self.expected.entry(pts).or_insert(0) += 1;
    }

    /// Withdraws one expectation, e.g. when the decoder reported the frame failed.
    pub fn cancel(&mut self, pts: i64) {
        if let Some(n) = self.expected.get_mut(&pts) {
            *n -= 1;
            if *n == 0 {
                self.expected.remove(&pts);
            }
        }
    }

    /// Adds a decoded frame.
    pub fn push(&mut self, pts: i64, item: T) {
        self.ready.insert((pts, self.seq), item);
        self.seq += 1;
    }

    /// The next frame in presentation order, if it is certain no earlier one can come.
    pub fn pop(&mut self) -> Option<(i64, T)> {
        let (&(pts, seq), _) = self.ready.first_key_value()?;
        let release = match self.mode {
            Mode::Depth(d) => self.ready.len() > d,
            Mode::Expected => match self.expected.first_key_value() {
                // Released when it is the earliest outstanding expectation, or when it
                // was never expected and is earlier than everything still to come.
                Some((&first, _)) => pts <= first,
                None => true,
            },
        };
        if !release {
            return None;
        }
        let item = self.ready.remove(&(pts, seq))?;
        if self.mode == Mode::Expected {
            self.cancel(pts);
        }
        Some((pts, item))
    }

    /// After the decoder flushed: every remaining frame in presentation order, and all
    /// expectations dropped (frames that never arrived are reported by the caller).
    pub fn drain(&mut self) -> Vec<(i64, T)> {
        self.expected.clear();
        std::mem::take(&mut self.ready)
            .into_iter()
            .map(|((pts, _), v)| (pts, v))
            .collect()
    }

    /// Forgets everything (seek).
    pub fn clear(&mut self) {
        self.expected.clear();
        self.ready.clear();
    }

    /// Frames buffered.
    pub fn len(&self) -> usize {
        self.ready.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    /// Expectations still outstanding.
    pub fn outstanding(&self) -> usize {
        self.expected.values().map(|&n| n as usize).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode order of an IBBP… GOP with B-pyramid: PTS 0 4 2 1 3 8 6 5 7.
    const DECODE_ORDER: [i64; 9] = [0, 4, 2, 1, 3, 8, 6, 5, 7];

    #[test]
    fn expected_mode_releases_in_pts_order_without_delay() {
        let mut r = ReorderStage::new();
        let mut out = Vec::new();
        for &p in &DECODE_ORDER {
            r.expect(p);
        }
        for &p in &DECODE_ORDER {
            r.push(p, p);
            while let Some((pts, _)) = r.pop() {
                out.push(pts);
            }
        }
        assert_eq!(out, (0..9).collect::<Vec<_>>());
        assert!(r.is_empty());
        assert_eq!(r.outstanding(), 0);
    }

    #[test]
    fn expected_mode_never_releases_early() {
        let mut r = ReorderStage::new();
        for &p in &DECODE_ORDER {
            r.expect(p);
        }
        r.push(0, ());
        assert_eq!(r.pop().map(|x| x.0), Some(0));
        r.push(4, ());
        r.push(2, ());
        // 1 is still expected: nothing comes out.
        assert!(r.pop().is_none());
        r.push(1, ());
        assert_eq!(r.pop().map(|x| x.0), Some(1));
        assert_eq!(r.pop().map(|x| x.0), Some(2));
        assert!(r.pop().is_none(), "3 is still expected");
    }

    #[test]
    fn cancel_unblocks() {
        let mut r = ReorderStage::new();
        r.expect(1);
        r.expect(2);
        r.push(2, ());
        assert!(r.pop().is_none());
        r.cancel(1);
        assert_eq!(r.pop().map(|x| x.0), Some(2));
    }

    #[test]
    fn duplicate_pts_both_released() {
        let mut r = ReorderStage::new();
        r.expect(5);
        r.expect(5);
        r.push(5, 'a');
        r.push(5, 'b');
        assert_eq!(r.pop(), Some((5, 'a')));
        assert_eq!(r.pop(), Some((5, 'b')));
        assert_eq!(r.outstanding(), 0);
    }

    #[test]
    fn depth_mode() {
        let mut r = ReorderStage::with_depth(2);
        let mut out = Vec::new();
        for &p in &DECODE_ORDER {
            r.push(p, ());
            while let Some((pts, _)) = r.pop() {
                out.push(pts);
            }
        }
        out.extend(r.drain().into_iter().map(|x| x.0));
        assert_eq!(out, (0..9).collect::<Vec<_>>());
    }
}
