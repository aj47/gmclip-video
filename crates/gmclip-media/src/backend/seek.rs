//! Frame-exact seek planning (SPEC §6.2, H-02): which samples to submit, in decode
//! order, so that exactly the target picture comes out of the decoder.
//!
//! The plan starts at the latest sync sample `k` at or before the target in decode
//! order whose PTS is not after the target's (a target with an earlier PTS than `k` is
//! a leading picture of `k` and may reference the previous GOP, so the plan steps back
//! one more sync sample). Every sample from `k` through the target is submitted, all
//! as pre-roll (`DoNotOutputFrame`) except the target — **except the leading pictures
//! of `k` itself**: in an open GOP they reference the GOP before `k`, which was not
//! decoded, and a sync sample is a recovery point exactly because nothing after it in
//! presentation order references them. Submitting them anyway makes VideoToolbox fail
//! later frames of the GOP with `kVTVideoDecoderBadDataErr` (measured on an x264
//! open-GOP clip, H-02).
//!
//! The H-02 review questioned the skip (a skipped reference B leaves a `frame_num` gap
//! that a decoder could mis-handle silently). Evidence: on the x264 open-GOP clip with
//! B-pyramid every one of 200 seeded targets decoded this way matched libavcodec's
//! md5, and the decoder fails the request if any pre-roll frame errors. None of AJ's
//! sources has leading pictures (0 in (a′), the MMO source and both proxies), so this
//! path serves foreign open-GOP files only.

use crate::index::{SampleIdx, SampleIndex};

/// One sample to submit.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct DecodeStep {
    pub sample: SampleIdx,
    /// False for pre-roll.
    pub output: bool,
}

fn pts(index: &SampleIndex, i: usize) -> i64 {
    index.dts[i] + i64::from(index.cts_off[i])
}

/// The sample whose PTS equals `target_pts`, if any.
pub fn sample_at_pts(index: &SampleIndex, target_pts: i64) -> Option<SampleIdx> {
    let order = &index.pts_order;
    let pos = order
        .binary_search_by(|&s| pts(index, s as usize).cmp(&target_pts))
        .ok()?;
    Some(SampleIdx(order[pos]))
}

/// The decode-order start (a sync sample) for decoding `target` frame-exactly.
pub fn seek_start(index: &SampleIndex, target: SampleIdx) -> Option<usize> {
    let t = target.0 as usize;
    if t >= index.len() {
        return None;
    }
    let target_pts = pts(index, t);
    let mut k = (0..=t).rev().find(|&i| index.sync[i])?;
    // A leading picture of `k` (earlier PTS, later decode) may reference the previous
    // GOP: start one sync sample earlier. With no earlier sync sample the target is an
    // undecodable leading picture of the file's first GOP: `None`.
    while pts(index, k) > target_pts {
        k = (0..k).rev().find(|&i| index.sync[i])?;
    }
    Some(k)
}

/// Whether sample `j` is a leading picture of sync sample `k`: later in decode order,
/// earlier in presentation order, with no sync sample in between.
pub fn is_leading_of(index: &SampleIndex, k: usize, j: usize) -> bool {
    j > k
        && j < index.len()
        && pts(index, j) < pts(index, k)
        && !index.sync[k + 1..=j].iter().any(|&s| s)
}

/// The full plan for decoding `target` from a fresh decoder session.
pub fn plan_seek(index: &SampleIndex, target: SampleIdx) -> Option<Vec<DecodeStep>> {
    let k = seek_start(index, target)?;
    let t = target.0 as usize;
    Some(
        (k..=t)
            .filter(|&i| i == t || !is_leading_of(index, k, i))
            .map(|i| DecodeStep {
                sample: SampleIdx(i as u32),
                output: i == t,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two closed GOPs of I P B B (decode order), 1 tick per frame, reorder depth 1:
    /// decode PTS 0 3 1 2 | 4 7 5 6.
    fn index() -> SampleIndex {
        let pts = [0i64, 3, 1, 2, 4, 7, 5, 6];
        let dts: Vec<i64> = (0..8).map(|i| i - 1).collect();
        let mut idx = SampleIndex {
            timescale: 30,
            dts: dts.clone(),
            cts_off: pts.iter().zip(&dts).map(|(p, d)| (p - d) as i32).collect(),
            offset: vec![0; 8],
            size: vec![1; 8],
            sync: vec![true, false, false, false, true, false, false, false],
            pts_order: vec![],
            reorder_depth: 1,
            cfr: None,
            complete: true,
        };
        let mut order: Vec<u32> = (0..8).collect();
        order.sort_by_key(|&s| pts[s as usize]);
        idx.pts_order = order;
        idx
    }

    #[test]
    fn finds_sample_by_pts() {
        let idx = index();
        assert_eq!(sample_at_pts(&idx, 2), Some(SampleIdx(3)));
        assert_eq!(sample_at_pts(&idx, 7), Some(SampleIdx(5)));
        assert_eq!(sample_at_pts(&idx, 8), None);
    }

    #[test]
    fn plan_preroll_then_target() {
        let idx = index();
        let plan = plan_seek(&idx, SampleIdx(6)).unwrap(); // PTS 5
        let s: Vec<(u32, bool)> = plan.iter().map(|d| (d.sample.0, d.output)).collect();
        assert_eq!(s, vec![(4, false), (5, false), (6, true)]);
        let plan = plan_seek(&idx, SampleIdx(0)).unwrap();
        assert_eq!(
            plan,
            vec![DecodeStep {
                sample: SampleIdx(0),
                output: true
            }]
        );
    }

    #[test]
    fn leading_picture_steps_back_a_gop() {
        // Open GOP: second I (decode 4) has PTS 6; decode 5 is a leading B with PTS 4.
        let pts = [0i64, 3, 1, 2, 6, 4, 5, 7];
        let mut idx = index();
        idx.cts_off = pts
            .iter()
            .zip(&idx.dts)
            .map(|(p, d)| (p - d) as i32)
            .collect();
        let mut order: Vec<u32> = (0..8).collect();
        order.sort_by_key(|&s| pts[s as usize]);
        idx.pts_order = order;
        assert_eq!(seek_start(&idx, SampleIdx(5)), Some(0));
        assert_eq!(seek_start(&idx, SampleIdx(7)), Some(4));
        assert!(is_leading_of(&idx, 4, 5));
        assert!(is_leading_of(&idx, 4, 6));
        assert!(!is_leading_of(&idx, 4, 7));
        assert!(!is_leading_of(&idx, 0, 5), "a sync sample in between");
        // Trailing target of the open GOP: its leading pictures (decode 5, 6) are skipped.
        let plan: Vec<u32> = plan_seek(&idx, SampleIdx(7))
            .unwrap()
            .iter()
            .map(|d| d.sample.0)
            .collect();
        assert_eq!(plan, vec![4, 7]);
        // Leading target: decoded from the previous GOP, the I of its own GOP included.
        let plan: Vec<u32> = plan_seek(&idx, SampleIdx(5))
            .unwrap()
            .iter()
            .map(|d| d.sample.0)
            .collect();
        assert_eq!(plan, vec![0, 1, 2, 3, 4, 5]);
    }
}
