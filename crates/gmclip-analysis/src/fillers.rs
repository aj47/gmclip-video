//! Filler words (SPEC §5.5): where the word is comes from ASR, where to cut comes from
//! the waveform.
//!
//! ASR places a word within a couple of tenths of a second: close enough to **find**
//! an `um`, nowhere near close enough to **cut** one. So every cut is anchored on a run
//! of sound between quiet-grid regions:
//!
//! - **island**: quiet on both sides; the run is the filler. Exact.
//! - **opens**: the filler starts a run that carries on into the next word (the common
//!   case). Anchored on the run's start; the end is where the filler's own level drops
//!   18 dB and stays down 60 ms ([`crate::edges::audible_end`]), before the next word's
//!   middle.
//! - **ends**: the filler closes a run that began with the previous word; anchored on
//!   the run's end, starting where the filler's own level rises
//!   ([`crate::edges::audible_start`]).
//! - **fused**: no run lines up, or the edge inside the run cannot be measured.
//!   Skipped and counted, never guessed from ASR times.
//!
//! Clean-room from SPEC §5.5 and the documented behaviour of AJ's own tightening
//! script: the same constants, so the two agree on AJ's streams.

use crate::edges::{EdgeMethod, audible_end, audible_start};
use crate::energy::Energy;
use crate::silence::QuietGrid;
use crate::span::{Span, ms};

/// English fillers (§5.5). "like", "ah" and "you know" are excluded on purpose: they
/// carry meaning too often.
pub const FILLERS_EN: &[&str] = &[
    "um", "umm", "ummm", "uh", "uhh", "uhhh", "uhm", "erm", "mm", "mhm",
];

/// The filler list for a BCP-47 language tag; `None` when the language has none yet.
pub fn fillers_for(lang: &str) -> Option<&'static [&'static str]> {
    let primary = lang.split(['-', '_']).next().unwrap_or("");
    match primary.to_ascii_lowercase().as_str() {
        "en" => Some(FILLERS_EN),
        _ => None,
    }
}

/// How far from an ASR edge a run edge may sit and still anchor it.
pub const SNAP_MS: u32 = 350;
/// No cut shorter than this.
pub const MIN_CUT_MS: u32 = 60;
/// Longer than this is not a filler.
pub const FILLER_MAX_MS: u32 = 2_000;

/// Lowercase, keep letters, apostrophes and spaces (the normalisation fillers and the
/// arbiter compare on).
pub fn normalise(word: &str) -> String {
    word.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == ' ')
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A transcript word as a sample span of the analysed buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WordSpan<'a> {
    pub text: &'a str,
    pub span: Span,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FillerAnchor {
    Island,
    Opens,
    Ends,
}

/// One filler to cut.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FillerCut {
    /// Index of the (first) filler word.
    pub word: usize,
    /// Words the filler spans (multi-word fillers).
    pub words: usize,
    pub cut: Span,
    pub anchor: FillerAnchor,
}

/// What a filler sweep would do: the cuts, and the fused fillers it skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FillerReport {
    pub cuts: Vec<FillerCut>,
    pub skipped: Vec<usize>,
}

/// Finds the fillers among `words` inside `span` and anchors each on the quiet grid.
/// `fine` is the 10 ms / 5 ms energy of the same buffer.
pub fn detect(
    words: &[WordSpan<'_>],
    list: &[&str],
    grid: &QuietGrid,
    fine: &Energy,
    span: Span,
) -> FillerReport {
    let rate = grid.rate;
    let snap = ms(SNAP_MS, rate);
    let min_cut = ms(MIN_CUT_MS, rate);
    let runs = grid.sound_runs(span);
    let singles: Vec<String> = list.iter().map(|w| normalise(w)).collect();
    let mut phrases: Vec<Vec<String>> = singles
        .iter()
        .filter(|w| w.contains(' '))
        .map(|w| w.split(' ').map(str::to_owned).collect())
        .collect();
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let norm: Vec<String> = words.iter().map(|w| normalise(w.text)).collect();

    let mut report = FillerReport::default();
    let mut i = 0;
    while i < words.len() {
        let mut hit = usize::from(!norm[i].is_empty() && singles.contains(&norm[i]));
        if hit == 0 {
            for p in &phrases {
                if norm.get(i..i + p.len()).is_some_and(|w| w == p.as_slice()) {
                    hit = p.len();
                    break;
                }
            }
        }
        if hit == 0 {
            i += 1;
            continue;
        }
        let (ws, we) = (words[i].span.start, words[i + hit - 1].span.end);
        if we <= span.start || ws >= span.end {
            i += hit;
            continue;
        }
        let prev_mid = i.checked_sub(1).map(|j| words[j].span.mid());
        let next_mid = words.get(i + hit).map(|w| w.span.mid());
        match anchor(&runs, fine, ws, we, prev_mid, next_mid, snap, min_cut, rate) {
            Some((cut, a)) => report.cuts.push(FillerCut {
                word: i,
                words: hit,
                cut,
                anchor: a,
            }),
            None => report.skipped.push(i),
        }
        i += hit;
    }
    report
}

/// Whether `x` lies strictly inside `r`.
fn holds(r: &Span, x: Option<usize>) -> bool {
    x.is_some_and(|x| r.start < x && x < r.end)
}

/// Anchors the filler ASR placed at `[ws, we]` on one run of sound, or `None` (fused:
/// skipped). Every run near the filler is classified; a run that holds a neighbouring
/// word's midpoint on the wrong side is never used (it would cut that word), and the
/// valid run overlapping the filler most wins. An edge the waveform cannot measure is
/// never guessed from ASR times: the filler is skipped instead (§5.5).
#[allow(clippy::too_many_arguments)]
fn anchor(
    runs: &[Span],
    fine: &Energy,
    ws: usize,
    we: usize,
    prev_mid: Option<usize>,
    next_mid: Option<usize>,
    snap: usize,
    min_cut: usize,
    rate: u32,
) -> Option<(Span, FillerAnchor)> {
    let dur = we.saturating_sub(ws);
    let max_len = ms(FILLER_MAX_MS, rate);
    let mut best: Option<(usize, Span, FillerAnchor)> = None;
    for r in runs {
        if r.end + snap < ws {
            continue;
        }
        if r.start > we + snap {
            break;
        }
        let starts = r.start.abs_diff(ws) <= snap;
        let ends = r.end.abs_diff(we) <= snap;
        let candidate = if starts
            && ends
            && r.len() <= dur + snap
            && !holds(r, prev_mid)
            && !holds(r, next_mid)
        {
            Some((*r, FillerAnchor::Island))
        } else if starts && !holds(r, prev_mid) {
            // the run carries on into the next word: the cut ends where the filler's own
            // level falls away, measured before the next word's middle
            let limit = next_mid.map_or(r.end, |m| m.min(r.end));
            audible_end(fine, Span::new(r.start, we.max(r.start + 1)), limit)
                .filter(|e| e.method == EdgeMethod::Measured)
                .map(|e| e.sample)
                .filter(|&stop| stop >= r.start + min_cut)
                .map(|stop| (Span::new(r.start, stop), FillerAnchor::Opens))
        } else if ends && !holds(r, next_mid) {
            // the run started with the previous word: the cut starts where the filler's
            // sound rises, measured after the previous word's middle
            let limit = prev_mid.map_or(r.start, |m| m.max(r.start));
            audible_start(
                fine,
                Span::new(ws.max(r.start), we.min(r.end).max(ws.max(r.start) + 1)),
                limit,
            )
            .filter(|e| e.method == EdgeMethod::Measured)
            .map(|e| e.sample)
            .filter(|&from| from + min_cut <= r.end)
            .map(|from| (Span::new(from, r.end), FillerAnchor::Ends))
        } else {
            None
        };
        if let Some((cut, a)) = candidate {
            if cut.len() > max_len {
                continue;
            }
            let overlap = cut.end.min(we).saturating_sub(cut.start.max(ws));
            if best.is_none_or(|(o, _, _)| overlap > o) {
                best = Some((overlap, cut, a));
            }
        }
    }
    best.map(|(_, cut, a)| (cut, a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::energy::Hop;

    const RATE: u32 = 16_000;

    fn at(m: u32) -> usize {
        ms(m, RATE)
    }

    fn signal(len_ms: u32, bursts: &[(u32, u32, f32)]) -> Vec<f32> {
        let mut x: Vec<f32> = (0..at(len_ms))
            .map(|i| 10f32.powf(-70.0 / 20.0) * 2f32.sqrt() * (i as f32 * 0.9).sin())
            .collect();
        for &(a, b, db) in bursts {
            let amp = 10f32.powf(db / 20.0) * 2f32.sqrt();
            for (i, v) in x.iter_mut().enumerate().take(at(b)).skip(at(a)) {
                *v = amp * (2.0 * std::f32::consts::PI * 150.0 * i as f32 / RATE as f32).sin();
            }
        }
        x
    }

    fn run(x: &[f32], words: &[WordSpan<'_>]) -> FillerReport {
        let e40 = Energy::at_hop(x, RATE, Hop::Ms40).unwrap();
        let fine = Energy::fine(x, RATE).unwrap();
        let span = Span::new(0, x.len());
        let grid = QuietGrid::for_span(&e40, span).unwrap();
        detect(words, FILLERS_EN, &grid, &fine, span)
    }

    #[test]
    fn normalises_and_knows_its_languages() {
        assert_eq!(normalise("Uh,"), "uh");
        assert_eq!(normalise(" Um..."), "um");
        assert_eq!(normalise("don't"), "don't");
        assert!(fillers_for("en-GB").is_some());
        assert!(fillers_for("de").is_none());
    }

    #[test]
    fn island_opens_and_fused() {
        // "so [uh] (gap) right" — the uh is an island
        // "[um]then"            — um opens a run that continues into "then"
        let x = signal(
            4000,
            &[
                (200, 500, -18.0),   // so
                (800, 1100, -22.0),  // uh (island)
                (1500, 1800, -18.0), // right
                (2400, 2700, -16.0), // um (opens)
                (2700, 3100, -38.0), // then (abutting, 22 dB quieter: a measurable drop)
            ],
        );
        let words = [
            WordSpan {
                text: "so",
                span: Span::new(at(220), at(480)),
            },
            WordSpan {
                text: "uh",
                span: Span::new(at(860), at(1000)),
            },
            WordSpan {
                text: "right",
                span: Span::new(at(1520), at(1760)),
            },
            WordSpan {
                text: "Um,",
                span: Span::new(at(2480), at(2600)),
            },
            WordSpan {
                text: "then",
                span: Span::new(at(2700), at(3080)),
            },
        ];
        let r = run(&x, &words);
        assert!(r.skipped.is_empty(), "{r:?}");
        assert_eq!(r.cuts.len(), 2, "{r:?}");
        let island = r.cuts[0];
        assert_eq!(island.anchor, FillerAnchor::Island);
        assert!(island.cut.start.abs_diff(at(800)) <= at(40));
        assert!(island.cut.end.abs_diff(at(1100)) <= at(40));
        let opens = r.cuts[1];
        assert_eq!(opens.anchor, FillerAnchor::Opens);
        assert!(opens.cut.start.abs_diff(at(2400)) <= at(40), "{opens:?}");
        // never past the next word's ASR start
        assert!(opens.cut.end <= at(2700), "{opens:?}");
    }

    #[test]
    fn a_neighbouring_word_is_never_cut() {
        // "so" then a short gap, then "um": the run holding "so" is within the snap
        // distance of the filler's ASR edges, but it holds "so"'s middle.
        let x = signal(
            2000,
            &[(100, 300, -18.0), (400, 650, -20.0), (1300, 1600, -18.0)],
        );
        let words = [
            WordSpan {
                text: "so",
                span: Span::new(at(100), at(300)),
            },
            WordSpan {
                text: "um",
                span: Span::new(at(400), at(650)),
            },
            WordSpan {
                text: "okay",
                span: Span::new(at(1300), at(1600)),
            },
        ];
        let r = run(&x, &words);
        assert_eq!(r.cuts.len(), 1, "{r:?}");
        assert_eq!(r.cuts[0].anchor, FillerAnchor::Island);
        assert!(r.cuts[0].cut.start.abs_diff(at(400)) <= at(40), "{r:?}");
        // "um" fused into a louder "then" with no level drop: skipped, not guessed
        let x = signal(3000, &[(400, 650, -24.0), (650, 1100, -16.0)]);
        let words = [
            WordSpan {
                text: "um",
                span: Span::new(at(420), at(640)),
            },
            WordSpan {
                text: "then",
                span: Span::new(at(660), at(1080)),
            },
        ];
        let r = run(&x, &words);
        assert_eq!(r.skipped, vec![0], "{r:?}");
    }

    #[test]
    fn a_filler_with_no_run_is_skipped() {
        // ASR says "uh" in the middle of a long loud run: fused, skipped
        let x = signal(3000, &[(200, 2500, -18.0)]);
        let words = [
            WordSpan {
                text: "well",
                span: Span::new(at(220), at(600)),
            },
            WordSpan {
                text: "uh",
                span: Span::new(at(1200), at(1300)),
            },
            WordSpan {
                text: "yes",
                span: Span::new(at(2000), at(2400)),
            },
        ];
        let r = run(&x, &words);
        assert_eq!(r.skipped, vec![1]);
        assert!(r.cuts.is_empty());
    }
}
