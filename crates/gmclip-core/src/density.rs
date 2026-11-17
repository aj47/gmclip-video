//! The local density gate (SPEC §10.3.9/11, backlog D.04): is a video's moment list
//! exhaustive, or did finding moments under-collect? Its numbers are held to the
//! reference ones on five of AJ's streams (`lab/fixtures/density/`).
//!
//! The rate is moments per minute of **active talk** (line durations plus the gaps
//! between lines shorter than [`DEAD_AIR_S`]), never wall-clock: a stream that waited
//! forty minutes on a build is not thin for it. Under [`FLOOR_PER_MIN`] the list is
//! flagged, with the largest stretches of real talk no moment covers — where a sweep
//! would look first.

/// One moment per 75 s of active talk is the rubric's midpoint (60–90 s).
pub const TARGET_PER_MIN: f64 = 60.0 / 75.0;
/// Below this it is under-collection rather than a quiet video (half the target).
pub const FLOOR_PER_MIN: f64 = 0.40;
/// A pause longer than this (s) is dead air, not talk somebody could have clipped.
pub const DEAD_AIR_S: f64 = 30.0;
/// An uncarded stretch is listed from this long (s) …
pub const STRETCH_MIN_S: f64 = 180.0;
/// … and this many words.
pub const STRETCH_MIN_WORDS: usize = 150;
/// A line within this many seconds of a moment counts as covered by it.
pub const SLACK_S: f64 = 15.0;
/// Lines closer than this (s) continue the same stretch.
pub const JOIN_S: f64 = 45.0;
/// How many stretches a report lists.
pub const STRETCHES_LISTED: usize = 5;

/// One transcript line: its times (s) and how many words it has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub words: usize,
}

/// A stretch of talk no moment covers.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Stretch {
    pub start_s: f64,
    pub end_s: f64,
    pub words: usize,
}

/// A video's density verdict.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Density {
    pub moments: usize,
    pub active_min: f64,
    pub per_min: f64,
    /// Minutes of active talk per moment.
    pub one_per_min: Option<f64>,
    /// Moments the target rate would give.
    pub target: i64,
    pub short_by: i64,
    pub floor: f64,
    /// At or above the floor.
    pub ok: bool,
    /// The largest uncarded stretches, longest first (at most [`STRETCHES_LISTED`]).
    pub stretches: Vec<Stretch>,
}

/// Talk time (s): line durations plus the short gaps between consecutive lines.
pub fn active_seconds(lines: &[Line]) -> f64 {
    let mut total = 0.0;
    for (i, l) in lines.iter().enumerate() {
        total += l.end - l.start;
        if let Some(n) = lines.get(i + 1) {
            let gap = n.start - l.end;
            if gap > 0.0 && gap < DEAD_AIR_S {
                total += gap;
            }
        }
    }
    total
}

/// Stretches of real talk no moment span covers (lines of four words or more, joined
/// while under [`JOIN_S`] apart), at least [`STRETCH_MIN_S`] long with
/// [`STRETCH_MIN_WORDS`] words, longest first.
pub fn uncarded(lines: &[Line], spans: &[(f64, f64)]) -> Vec<Stretch> {
    let mut gaps: Vec<Stretch> = Vec::new();
    let mut cur: Option<Stretch> = None;
    for l in lines {
        let covered = spans
            .iter()
            .any(|&(s, e)| s - SLACK_S <= l.start && l.start <= e + SLACK_S);
        if covered || l.words < 4 {
            gaps.extend(cur.take());
        } else {
            match &mut cur {
                Some(c) if l.start - c.end_s < JOIN_S => {
                    c.end_s = l.end;
                    c.words += l.words;
                }
                _ => {
                    gaps.extend(cur.take());
                    cur = Some(Stretch {
                        start_s: l.start,
                        end_s: l.end,
                        words: l.words,
                    });
                }
            }
        }
    }
    gaps.extend(cur);
    gaps.retain(|g| g.end_s - g.start_s >= STRETCH_MIN_S && g.words >= STRETCH_MIN_WORDS);
    // longest first; equal lengths keep their order (Python's sort is stable)
    gaps.sort_by(|a, b| (b.end_s - b.start_s).total_cmp(&(a.end_s - a.start_s)));
    gaps
}

/// The verdict for a video with these transcript lines (in time order) and these
/// moment spans (s). `None` without lines (nothing to measure against yet).
pub fn density(lines: &[Line], spans: &[(f64, f64)]) -> Option<Density> {
    if lines.is_empty() {
        return None;
    }
    let active_min = active_seconds(lines) / 60.0;
    let n = spans.len();
    let per_min = if active_min > 0.0 {
        n as f64 / active_min
    } else {
        0.0
    };
    let target = (active_min * TARGET_PER_MIN).round_ties_even() as i64;
    let mut stretches = uncarded(lines, spans);
    stretches.truncate(STRETCHES_LISTED);
    Some(Density {
        moments: n,
        active_min,
        per_min,
        one_per_min: (per_min > 0.0).then(|| 1.0 / per_min),
        target,
        short_by: (target - n as i64).max(0),
        floor: FLOOR_PER_MIN,
        ok: per_min >= FLOOR_PER_MIN,
        stretches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, end: f64, words: usize) -> Line {
        Line { start, end, words }
    }

    #[test]
    fn active_talk_skips_dead_air() {
        // 10 s + 5 s gap + 10 s, then 60 s of dead air, then 10 s
        let l = [
            line(0.0, 10.0, 20),
            line(15.0, 25.0, 20),
            line(85.0, 95.0, 20),
        ];
        assert_eq!(active_seconds(&l), 35.0);
    }

    #[test]
    fn the_floor_and_the_stretches() {
        // 20 minutes of steady talk: one line per 10 s, 25 words each
        let l: Vec<Line> = (0..120)
            .map(|i| line(f64::from(i) * 10.0, f64::from(i) * 10.0 + 9.0, 25))
            .collect();
        // one moment at the start: 0.05 per active minute, flagged
        let d = density(&l, &[(0.0, 30.0)]).unwrap();
        assert!((d.active_min - 1199.0 / 60.0).abs() < 1e-9);
        assert!(!d.ok);
        assert_eq!(d.target, 16);
        assert_eq!(d.short_by, 15);
        // everything after the moment's slack is one uncarded stretch
        assert_eq!(d.stretches.len(), 1);
        assert_eq!(d.stretches[0].start_s, 50.0);
        assert_eq!(d.stretches[0].end_s, 1199.0);
        // twelve moments spread out: 0.6 per minute, no stretch left
        let spans: Vec<(f64, f64)> = (0..12)
            .map(|i| (f64::from(i) * 100.0, f64::from(i) * 100.0 + 75.0))
            .collect();
        let d = density(&l, &spans).unwrap();
        assert!(d.ok, "{d:?}");
        assert!(d.stretches.is_empty());
    }

    #[test]
    fn short_lines_and_long_pauses_break_a_stretch() {
        let mut l: Vec<Line> = (0..30)
            .map(|i| line(f64::from(i) * 10.0, f64::from(i) * 10.0 + 9.0, 10))
            .collect();
        // a three-word line ends the first stretch; a 50 s pause starts another
        l.push(line(300.0, 302.0, 3));
        l.extend((0..30).map(|i| line(352.0 + f64::from(i) * 7.0, 358.0 + f64::from(i) * 7.0, 6)));
        let g = uncarded(&l, &[]);
        assert_eq!(g.len(), 2, "{g:?}");
        assert_eq!((g[0].start_s, g[0].end_s, g[0].words), (0.0, 299.0, 300));
        assert_eq!((g[1].start_s, g[1].words), (352.0, 180));
        assert!(density(&[], &[]).is_none());
    }
}
