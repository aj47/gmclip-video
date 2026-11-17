//! The hand-off from an OS video encoder to a muxer (SPEC §6.5 steps 3–4), and the
//! encoder-DTS rebase.
//!
//! With B-frames an encoder emits decode timestamps that can run *ahead* of the
//! presentation timestamps of the frames they belong to (VideoToolbox stamps the
//! first frame DTS = PTS = 0 and then counts DTS up while B-frames show earlier).
//! Muxing that as-is gives DTS > PTS. [`DtsRebaser`] measures the encoder's reorder
//! delay on the first GOP — the largest `dts − pts` there — and shifts every DTS down
//! by it, so `dts ≤ pts` for every sample while the first PTS stays 0
//! (implementer finding 15; FFmpeg's `videotoolboxenc` does the same).

use crate::decode::MediaError;

/// One compressed video sample in decode order, timestamps in the stream timescale.
#[derive(Copy, Clone, Debug)]
pub struct EncodedVideoSample<'a> {
    /// AVCC: 4-byte big-endian length-prefixed NAL units.
    pub data: &'a [u8],
    pub pts: i64,
    /// Rebased: `dts ≤ pts`, strictly increasing.
    pub dts: i64,
    /// `next.dts − dts` (the last sample: its encoder duration), always > 0 — what
    /// an `stts` entry needs.
    pub duration: u32,
    pub sync: bool,
}

/// Where encoded video goes: Lane B's `Mp4Writer` (B-04), or a test sink.
pub trait VideoSampleSink {
    /// The decoder configuration record (`avcC`), before the first sample.
    fn video_config(&mut self, avcc: &[u8]) -> Result<(), MediaError>;
    fn video_sample(&mut self, s: &EncodedVideoSample<'_>) -> Result<(), MediaError>;
}

/// The largest number of bits in any window of `window` ticks (decode order, by DTS),
/// as the MP4 `btrt` max bitrate and the platforms count it. With a 1-s window this
/// is the 1-s peak in bits per second.
#[derive(Debug)]
pub struct PeakMeter {
    window: i64,
    q: std::collections::VecDeque<(i64, u64)>,
    sum: u64,
    peak: u64,
}

impl PeakMeter {
    pub fn new(window_ticks: i64) -> Self {
        Self {
            window: window_ticks.max(1),
            q: Default::default(),
            sum: 0,
            peak: 0,
        }
    }

    pub fn push(&mut self, dts: i64, bytes: usize) {
        let bits = bytes as u64 * 8;
        self.q.push_back((dts, bits));
        self.sum += bits;
        while let Some(&(d, b)) = self.q.front() {
            if d <= dts - self.window {
                self.q.pop_front();
                self.sum -= b;
            } else {
                break;
            }
        }
        self.peak = self.peak.max(self.sum);
    }

    /// Bits in the fullest window so far.
    pub fn peak(&self) -> u64 {
        self.peak
    }
}

/// SPEC §6.5.1: social presets never exceed 25 Mbps in any 1-s window.
pub const SOCIAL_PEAK_BPS: u64 = 25_000_000;

/// The retry ladder when an encode breaks the 1-s peak: the same encode with a
/// rising `MinAllowedFrameQP`. VideoToolbox's rate control overshoots for about a
/// second after a cut from easy to hard content (H-08 review: 35 Mbps at a
/// 16/24 Mbps setting); a QP floor stops that burst, but costs quality on easy
/// content, so it is used only on retry. H-10 owns the retry loop.
pub const RATE_CAP_LADDER: [Option<u8>; 4] = [None, Some(24), Some(28), Some(32)];

/// Where encoded audio goes (Lane B's `Mp4Writer`, or a test sink). Timestamps are in
/// samples; the first packet is at 0 and there is no edit list (SPEC §6.5 step 3).
pub trait AudioSampleSink {
    /// The AudioSpecificConfig, before the first packet.
    fn audio_config(
        &mut self,
        asc: &[u8],
        sample_rate: u32,
        channels: u32,
    ) -> Result<(), MediaError>;
    fn audio_sample(&mut self, data: &[u8], pts: i64, duration: u32) -> Result<(), MediaError>;
}

/// A packet as an encoder produced it.
#[derive(Clone, Debug)]
pub struct RawPacket {
    pub data: Vec<u8>,
    pub pts: i64,
    /// `None` when the encoder did not stamp one (allowed only without reordering).
    pub dts: Option<i64>,
    pub duration: Option<i64>,
    pub keyframe: bool,
}

fn err(step: &'static str, frame: Option<u64>) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: None,
    }
}

/// Buffers the first GOP, measures the reorder delay, then passes packets through
/// with `dts − delay`; durations are the DTS gaps (one sample of lag).
#[derive(Debug)]
pub struct DtsRebaser {
    buffered: Vec<RawPacket>,
    delay: Option<i64>,
    /// Lower bound for the delay (configured reorder depth × frame duration), so a
    /// later GOP needing as much as the encoder may use never fails the export.
    min_delay: i64,
    reordering: bool,
    held: Option<(RawPacket, i64)>,
    last_dts: Option<i64>,
    count: u64,
}

impl DtsRebaser {
    /// `reordering`: B-frames are on (a missing DTS is then an error); `min_delay`:
    /// the configured reorder depth × frame duration, in ticks.
    pub fn new(reordering: bool, min_delay: i64) -> Self {
        Self {
            buffered: Vec::new(),
            delay: None,
            min_delay: min_delay.max(0),
            reordering,
            held: None,
            last_dts: None,
            count: 0,
        }
    }

    /// The delay in use, once the first GOP is complete.
    pub fn delay(&self) -> Option<i64> {
        self.delay
    }

    fn out_one(
        &mut self,
        p: RawPacket,
        dts: i64,
        duration: i64,
        out: &mut dyn FnMut(EncodedVideoSample<'_>) -> Result<(), MediaError>,
    ) -> Result<(), MediaError> {
        let duration = u32::try_from(duration)
            .ok()
            .filter(|&d| d > 0)
            .ok_or(err("dts_rebase_bad_duration", Some(self.count)))?;
        self.count += 1;
        out(EncodedVideoSample {
            data: &p.data,
            pts: p.pts,
            dts,
            duration,
            sync: p.keyframe,
        })
    }

    fn emit(
        &mut self,
        p: RawPacket,
        delay: i64,
        out: &mut dyn FnMut(EncodedVideoSample<'_>) -> Result<(), MediaError>,
    ) -> Result<(), MediaError> {
        let raw = match p.dts {
            Some(d) => d,
            None if self.reordering => return Err(err("dts_rebase_missing_dts", Some(self.count))),
            None => p.pts,
        };
        let dts = raw - delay;
        if dts > p.pts {
            return Err(err("dts_rebase_dts_after_pts", Some(self.count)));
        }
        if self.last_dts.is_some_and(|l| dts <= l) {
            return Err(err("dts_rebase_not_increasing", Some(self.count)));
        }
        self.last_dts = Some(dts);
        if let Some((prev, prev_dts)) = self.held.take() {
            self.out_one(prev, prev_dts, dts - prev_dts, out)?;
        }
        self.held = Some((p, dts));
        Ok(())
    }

    fn settle(
        &mut self,
        out: &mut dyn FnMut(EncodedVideoSample<'_>) -> Result<(), MediaError>,
    ) -> Result<(), MediaError> {
        let measured = self
            .buffered
            .iter()
            .map(|p| p.dts.unwrap_or(p.pts) - p.pts)
            .max()
            .unwrap_or(0);
        let delay = measured.max(self.min_delay).max(0);
        self.delay = Some(delay);
        for p in std::mem::take(&mut self.buffered) {
            self.emit(p, delay, out)?;
        }
        Ok(())
    }

    /// Feeds one packet (decode order); emits zero or more rebased samples.
    pub fn push(
        &mut self,
        p: RawPacket,
        out: &mut dyn FnMut(EncodedVideoSample<'_>) -> Result<(), MediaError>,
    ) -> Result<(), MediaError> {
        if let Some(d) = self.delay {
            return self.emit(p, d, out);
        }
        // The first GOP ends at the second keyframe.
        if p.keyframe && !self.buffered.is_empty() {
            self.settle(out)?;
            let d = self.delay.unwrap_or(0);
            return self.emit(p, d, out);
        }
        self.buffered.push(p);
        Ok(())
    }

    /// End of stream: flushes a stream shorter than one GOP, and the last sample
    /// (its duration from the encoder).
    pub fn finish(
        &mut self,
        out: &mut dyn FnMut(EncodedVideoSample<'_>) -> Result<(), MediaError>,
    ) -> Result<(), MediaError> {
        if self.delay.is_none() {
            self.settle(out)?;
        }
        if let Some((p, dts)) = self.held.take() {
            let d = p
                .duration
                .ok_or(err("dts_rebase_last_duration", Some(self.count)))?;
            self.out_one(p, dts, d, out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(pts: i64, dts: i64, key: bool) -> RawPacket {
        RawPacket {
            data: vec![0],
            pts,
            dts: Some(dts),
            duration: Some(1),
            keyframe: key,
        }
    }

    fn run(r: &mut DtsRebaser, input: Vec<RawPacket>) -> Result<Vec<(i64, i64, u32)>, MediaError> {
        let mut got = Vec::new();
        let mut out = |s: EncodedVideoSample<'_>| {
            got.push((s.pts, s.dts, s.duration));
            Ok(())
        };
        for p in input {
            r.push(p, &mut out)?;
        }
        r.finish(&mut out)?;
        Ok(got)
    }

    /// VideoToolbox stamps with B=2, closed GOPs (I0 P3 B1 B2 | I4 P7 B5 B6), DTS
    /// counting 0, 1, 2, … from the first PTS (measured: delay 2 frames at B=2).
    #[test]
    fn rebases_by_first_gop_delay() {
        let input = vec![
            pk(0, 0, true),
            pk(3, 1, false),
            pk(1, 2, false),
            pk(2, 3, false),
            pk(4, 4, true),
            pk(7, 5, false),
            pk(5, 6, false),
            pk(6, 7, false),
        ];
        let mut r = DtsRebaser::new(true, 0);
        let got = run(&mut r, input).unwrap();
        assert_eq!(r.delay(), Some(1));
        assert_eq!(got[0].0, 0, "first PTS stays 0");
        assert!(got.iter().all(|(p, d, _)| d <= p));
        assert!(
            got.windows(2)
                .all(|w| w[0].1 < w[1].1 && w[0].1 + i64::from(w[0].2) == w[1].1)
        );
        assert_eq!(got.len(), 8);
    }

    #[test]
    fn min_delay_floor_applies() {
        let mut r = DtsRebaser::new(true, 2);
        let got = run(
            &mut r,
            vec![pk(0, 0, true), pk(1, 1, false), pk(2, 2, true)],
        )
        .unwrap();
        assert_eq!(r.delay(), Some(2));
        assert_eq!(got[0].1, -2);
    }

    #[test]
    fn no_reordering_is_a_no_op() {
        let mut r = DtsRebaser::new(false, 0);
        let got = run(&mut r, (0..5).map(|i| pk(i, i, i == 0)).collect()).unwrap();
        assert_eq!(got, (0..5).map(|i| (i, i, 1)).collect::<Vec<_>>());
    }

    #[test]
    fn later_violation_is_an_error() {
        let mut r = DtsRebaser::new(true, 0);
        let e = run(
            &mut r,
            vec![pk(0, 0, true), pk(1, 1, true), pk(2, 5, false)],
        )
        .unwrap_err();
        assert_eq!(e.step, "dts_rebase_dts_after_pts");
    }

    #[test]
    fn missing_dts_with_reordering_is_an_error() {
        let mut r = DtsRebaser::new(true, 0);
        let mut p = pk(1, 1, true);
        p.dts = None;
        let e = run(&mut r, vec![pk(0, 0, true), p]).unwrap_err();
        assert_eq!(e.step, "dts_rebase_missing_dts");
    }

    #[test]
    fn peak_meter_counts_decode_order_windows() {
        let mut m = PeakMeter::new(30);
        for i in 0..90 {
            m.push(i, if (40..45).contains(&i) { 1000 } else { 10 });
        }
        // Window of 30 ticks containing the 5 big samples + 25 small ones.
        assert_eq!(m.peak(), (5 * 1000 + 25 * 10) * 8);
    }
}
