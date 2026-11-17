//! Export verify v0 (B-15; SPEC R-EXP-6, R-EXP-2's measurable parts).
//!
//! [`container`] re-demuxes the written file (our own [`crate::mux::inspect`] parser until
//! B-01's index lands) and checks codec, profile/level, canvas, SAR, frame count, the
//! iTunSMPB gapless tag (priming 0, decision 0004), durations (video ±½ frame; audio
//! exact to the sample, R-TIME-2), no `edts`, first PTS 0 and BT.709 tags. [`audio`]
//! takes the file's audio as the OS decoded it (Lane H's decoder; AVFoundation or ffmpeg
//! in the lab) and the planned mix (B-11) and checks: decoded length, first-sample
//! alignment (xcorr ≤ 1 ms), the A/V offset at every discontinuous join (xcorr over a
//! ±0.5 s window *and* over the part that starts there, so a short misplaced part is not
//! averaged away; ≤ ½ frame and ≤ 10 ms), the drift of those offsets (slope ≤ 0.1
//! ms/join) and loudness of the delivered audio (±0.5 LU, SPEC G14; true peak ≤ the
//! ceiling + 0.1 dB between decoders). Every failed check names its `step`, which
//! [`VerifyReport::first_failure`] turns into a `MediaError`.
//!
//! Lags are searched over ±2400 samples (±1.5 frames at 30 fps, wider than AAC priming),
//! coarse on 8× block-averaged signals, then exact within ±16 samples. On strongly periodic content a correlation peak can
//! alias by a period; speech and program material do not do this in practice.

use crate::MediaError;
use crate::audio::graph::{AudioPlan, LoudnessMode, RATE};
use crate::audio::loudness;
use crate::mux::inspect;

/// H.264 levels (Table A-1) as written in a plan → `level_idc`.
fn level_idc(level: &str) -> Option<u8> {
    Some(match level {
        "1" | "1.0" => 10,
        "1b" => 9,
        "1.1" => 11,
        "1.2" => 12,
        "1.3" => 13,
        "2" | "2.0" => 20,
        "2.1" => 21,
        "2.2" => 22,
        "3" | "3.0" => 30,
        "3.1" => 31,
        "3.2" => 32,
        "4" | "4.0" => 40,
        "4.1" => 41,
        "4.2" => 42,
        "5" | "5.0" => 50,
        "5.1" => 51,
        "5.2" => 52,
        "6" | "6.0" => 60,
        "6.1" => 61,
        "6.2" => 62,
        _ => return None,
    })
}

/// What the plan says the file must be.
#[derive(Clone, Debug, PartialEq)]
pub struct Expect {
    pub frames: u64,
    /// The output rate `fps / fps_den` (1, or 1001 for an NTSC rate, decision 0020).
    pub fps: u32,
    pub fps_den: u32,
    pub width: u16,
    pub height: u16,
    /// H.264 `profile_idc` (High = 100), when the plan says.
    pub profile_idc: Option<u8>,
    /// The plan's level string, when it says (checked against Table A-1).
    pub level: Option<String>,
    /// Full-range video (`output.color.range == full`); limited otherwise.
    pub full_range: bool,
    pub loudness: LoudnessMode,
}

impl Expect {
    /// From a typed RenderPlan v0.
    #[must_use]
    pub fn from_plan(p: &gmclip_plan::Plan) -> Self {
        use gmclip_plan::types::{ColorRange, VideoProfile};
        let profile_idc = p.output.video.profile.map(|v| match v {
            VideoProfile::High => 100,
            VideoProfile::Main => 77,
            VideoProfile::ConstrainedBaseline => 66,
        });
        let loudness = match p.audio.loudness {
            gmclip_plan::types::Loudness::AsRecorded => LoudnessMode::AsRecorded,
            gmclip_plan::types::Loudness::Static {
                integrated_lufs,
                true_peak_dbtp,
            } => LoudnessMode::Static {
                integrated_lufs,
                true_peak_dbtp,
            },
        };
        Self {
            frames: p.output.frames,
            fps: u32::try_from(p.output.fps.num.get()).unwrap_or(30),
            fps_den: u32::try_from(p.output.fps.den.get()).unwrap_or(1),
            width: u16::try_from(p.output.canvas.w.get()).unwrap_or(0),
            height: u16::try_from(p.output.canvas.h.get()).unwrap_or(0),
            profile_idc,
            level: p.output.video.level.clone(),
            full_range: p.output.color.as_ref().and_then(|c| c.range) == Some(ColorRange::Full),
            loudness,
        }
    }

    /// Whole samples per frame (1601 at 29.97, whose frames carry 1601 or 1602).
    fn samples_per_frame(&self) -> u64 {
        u64::from(RATE) * u64::from(self.fps_den.max(1)) / u64::from(self.fps.max(1))
    }

    /// `S(frames)`: the audio the plan delivers (rule 4, rounding down at NTSC rates).
    fn total_samples(&self) -> u64 {
        let s = u128::from(self.frames) * u128::from(RATE) * u128::from(self.fps_den.max(1));
        u64::try_from(s / u128::from(self.fps.max(1))).unwrap_or(u64::MAX)
    }

    /// Frames per second.
    fn rate(&self) -> f64 {
        f64::from(self.fps.max(1)) / f64::from(self.fps_den.max(1))
    }
}

/// One check's outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub step: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn check(step: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        step,
        ok,
        detail: detail.into(),
    }
}

/// The measured A/V offset at one join.
#[derive(Clone, Debug, PartialEq)]
pub struct JoinOffset {
    /// Index of the part after the join.
    pub part: usize,
    /// Output sample of the join.
    pub at: u64,
    /// Where the planned audio sits in the decoded audio over the ±0.5 s window:
    /// `decoded[q + lag] ≈ planned[q]`.
    pub lag_samples: i64,
    pub lag_ms: f64,
    pub corr: f64,
    /// The same over the part that starts at the join (up to 0.5 s of it), when it is
    /// at least one frame long.
    pub part_lag_samples: Option<i64>,
    pub ok: bool,
}

/// Everything verify measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VerifyReport {
    pub checks: Vec<Check>,
    pub joins: Vec<JoinOffset>,
    /// Least-squares slope of join offset (ms) against join index.
    pub drift_ms_per_join: Option<f64>,
}

impl VerifyReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// The first failed check as a `MediaError` naming its step (R-EXP-4's shape).
    #[must_use]
    pub fn first_failure(&self) -> Option<MediaError> {
        self.checks.iter().find(|c| !c.ok).map(|c| MediaError {
            step: c.step,
            frame: None,
            source_id: None,
            os_status: None,
        })
    }
}

/// Container checks on the written file's bytes.
#[must_use]
pub fn container(file: &[u8], e: &Expect) -> Vec<Check> {
    let mut out = Vec::new();
    let tracks = match inspect::tracks(file) {
        Ok(t) => t,
        Err(err) => return vec![check("demux", false, err.to_string())],
    };
    let video: Vec<_> = tracks.iter().filter(|t| &t.handler == b"vide").collect();
    let audio: Vec<_> = tracks.iter().filter(|t| &t.handler == b"soun").collect();
    let (Some(v), Some(a)) = (video.first(), audio.first()) else {
        return vec![check("demux", false, "needs one video and one audio track")];
    };
    out.push(check(
        "demux",
        video.len() == 1 && audio.len() == 1 && tracks.len() == 2,
        format!(
            "{} video, {} audio, {} tracks",
            video.len(),
            audio.len(),
            tracks.len()
        ),
    ));

    // Codec: avc1 + mp4a carrying AAC-LC (AudioSpecificConfig object type 2, no escape),
    // 48 kHz (frequency index 3), stereo (channel configuration 2).
    let asc = &a.entry.config;
    let aot = asc.first().map_or(0, |b| b >> 3);
    let freq_index = match (asc.first(), asc.get(1)) {
        (Some(x), Some(y)) => ((x & 0x07) << 1) | (y >> 7),
        _ => 0xff,
    };
    let chan_cfg = asc.get(1).map_or(0, |y| (y >> 3) & 0x0f);
    out.push(check(
        "codec",
        &v.sample_entry == b"avc1"
            && &a.sample_entry == b"mp4a"
            && aot == 2
            && freq_index == 3
            && chan_cfg == 2,
        format!(
            "video {}, audio {} (AOT {aot}, rate index {freq_index}, channels {chan_cfg})",
            String::from_utf8_lossy(&v.sample_entry),
            String::from_utf8_lossy(&a.sample_entry)
        ),
    ));
    // Profile / level from the avcC record header (copied verbatim from the encoder).
    let (profile, level) = (
        v.entry.config.get(1).copied(),
        v.entry.config.get(3).copied(),
    );
    let want_level = e.level.as_deref().map(level_idc);
    out.push(check(
        "profile",
        e.profile_idc.is_none_or(|p| profile == Some(p))
            && match want_level {
                None => true,
                Some(None) => false, // the plan names a level H.264 does not have
                Some(Some(l)) => level == Some(l),
            },
        format!(
            "profile_idc {profile:?} level_idc {level:?} (want {:?} / {:?})",
            e.profile_idc, e.level
        ),
    ));
    out.push(check(
        "canvas",
        (v.entry.width, v.entry.height) == (e.width, e.height),
        format!(
            "{}x{} (want {}x{})",
            v.entry.width, v.entry.height, e.width, e.height
        ),
    ));
    // Our writer always writes `pasp`; the SPS VUI is read once B-03 lands.
    out.push(match v.entry.pasp {
        Some((h, w)) => check("sar", h == w && h > 0, format!("pasp {h}:{w}")),
        None => check("sar", false, "no pasp"),
    });
    out.push(check(
        "frames",
        v.samples.len() as u64 == e.frames,
        format!("{} video samples (plan {})", v.samples.len(), e.frames),
    ));
    // The gapless tag before durations: a missing tag is the cause, a long track the
    // symptom. Decision 0004: priming 0; the track holds the length and at most the
    // padding past it.
    let gapless = inspect::itunsmpb(file).ok().flatten();
    let a_media: u64 = a.samples.iter().map(|s| u64::from(s.duration)).sum();
    out.push(check(
        "gapless",
        gapless.is_some_and(|g| {
            g.priming == 0 && a_media >= g.length && a_media <= g.length + u64::from(g.padding)
        }),
        format!("iTunSMPB {gapless:?}, audio track {a_media} samples"),
    ));
    // Durations: video ±½ frame on its own clock (sum of sample durations); audio as
    // presented (the tag's length) exactly S(frames) (R-TIME-2: sample precision).
    let half_frame_s = 0.5 / e.rate();
    let want_s = e.frames as f64 / e.rate();
    let v_media: u64 = v.samples.iter().map(|s| u64::from(s.duration)).sum();
    let v_s = v_media as f64 / f64::from(v.timescale.max(1));
    let a_len = gapless.map_or(a_media, |g| g.length);
    let want_samples = e.total_samples();
    out.push(check(
        "duration",
        (v_s - want_s).abs() <= half_frame_s && a_len == want_samples && a.timescale == RATE,
        format!("video {v_s:.6} s (plan {want_s:.6} ±{half_frame_s:.4}), audio {a_len} samples (plan {want_samples})"),
    ));
    let edts: Vec<u32> = tracks.iter().filter(|t| t.has_edts).map(|t| t.id).collect();
    out.push(check(
        "edts",
        edts.is_empty(),
        if edts.is_empty() {
            "none".to_owned()
        } else {
            format!("tracks {edts:?} have edit lists")
        },
    ));
    let v_first = v.samples.iter().map(|s| s.pts).min();
    out.push(check(
        "first_pts",
        v_first == Some(0),
        format!("first video PTS {v_first:?}"),
    ));
    out.push(check(
        "colour",
        v.entry.colr == Some((1, 1, 1, e.full_range)),
        format!(
            "colr {:?} (want BT.709 1/1/1, full range {})",
            v.entry.colr, e.full_range
        ),
    ));
    out
}

/// Interleaved stereo → mono (L+R)/2 in f64.
fn mono(x: &[f32]) -> Vec<f64> {
    x.chunks_exact(2)
        .map(|f| (f64::from(f[0]) + f64::from(f[1])) / 2.0)
        .collect()
}

/// Normalised correlation of `got[q + lag]` with `want[q]` over `q` in `[from, to)`,
/// stepping `q` by `step`.
fn corr_at(want: &[f64], got: &[f64], from: usize, to: usize, lag: i64, step: usize) -> f64 {
    let (mut dot, mut ew, mut eg) = (0.0, 0.0, 0.0);
    let mut q = from;
    while q < to {
        let j = q as i64 + lag;
        if let (Some(w), Some(g)) = (
            want.get(q),
            usize::try_from(j).ok().and_then(|j| got.get(j)),
        ) {
            dot += w * g;
            ew += w * w;
            eg += g * g;
        }
        q += step;
    }
    if ew > 0.0 && eg > 0.0 {
        dot / (ew * eg).sqrt()
    } else {
        0.0
    }
}

/// Block means of `x` over 8 samples: a low-passed, decimated copy whose correlation
/// peak is wide enough for a coarse search.
fn decimate8(x: &[f64]) -> Vec<f64> {
    x.chunks(8)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

/// The lag in `[-max, max]` maximising the correlation over `[from, to)`: coarse on 8×
/// decimated signals (every lag of the decimated clock), then exact within ±16 samples of
/// the coarse best.
fn xcorr(want: &[f64], got: &[f64], from: usize, to: usize, max: i64) -> (i64, f64) {
    let to = to.min(want.len());
    if from >= to {
        return (0, 0.0);
    }
    let (w8, g8) = (decimate8(want), decimate8(got));
    let mut coarse = (0i64, f64::MIN);
    for lag in -(max / 8)..=(max / 8) {
        let c = corr_at(&w8, &g8, from / 8, to.div_ceil(8), lag, 1);
        if c > coarse.1 {
            coarse = (lag, c);
        }
    }
    let centre = coarse.0 * 8;
    let mut best = (0i64, f64::MIN);
    for lag in (centre - 16).max(-max)..=(centre + 16).min(max) {
        let c = corr_at(want, got, from, to, lag, 1);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

/// Search range for every lag measurement (samples).
pub const MAX_LAG: i64 = 2400;
/// Skip the encoder warm-up and the 5 ms feed fade at the start (≤ 2112 + 240 samples).
const WARMUP: usize = 4_800;

/// Audio checks: `decoded` is the file's audio as the OS decoded it (48 kHz interleaved
/// stereo, gapless-trimmed where the decoder honours the tag), `planned` the plan's mix
/// (B-11, before loudness).
#[must_use]
pub fn audio(
    decoded: &[f32],
    planned: &[f32],
    plan: &AudioPlan,
    e: &Expect,
) -> (Vec<Check>, Vec<JoinOffset>, Option<f64>) {
    let mut out = Vec::new();
    let (d, p) = (mono(decoded), mono(planned));
    let spf = e.samples_per_frame() as usize;
    // Decoded length: at least the plan, at most one AAC unit of trailing padding (ffmpeg
    // keeps it; AVFoundation trims it).
    out.push(check(
        "audio_length",
        d.len() >= p.len() && d.len() <= p.len() + 1024,
        format!("decoded {} samples, planned {}", d.len(), p.len()),
    ));
    // First-sample alignment over the first 0.5 s past the warm-up.
    let (lag, corr) = xcorr(&p, &d, WARMUP, (WARMUP + 24_000).min(p.len()), MAX_LAG);
    out.push(check(
        "audio_sync",
        lag.unsigned_abs() <= 48 && corr > 0.9,
        format!(
            "first-window lag {lag} samples ({:.2} ms), corr {corr:.4}",
            lag as f64 / 48.0
        ),
    ));
    // A/V offset at every discontinuous join: ±0.5 s window, and the part that starts
    // there on its own (a short misplaced part barely moves a 1 s window).
    let limit = (spf as u64 / 2).min(480); // ≤ ½ frame and ≤ 10 ms
    let mut joins = Vec::new();
    let all = plan.joins();
    for (k, j) in all.iter().enumerate().filter(|(_, j)| !j.continuous) {
        let at = j.at as usize;
        let (a, b) = (
            at.saturating_sub(24_000).max(WARMUP),
            (at + 24_000).min(p.len()),
        );
        let (lag, corr) = xcorr(&p, &d, a, b, MAX_LAG);
        let next = all
            .get(k + 1)
            .map_or(p.len(), |n| n.at as usize)
            .min(at + 24_000)
            .min(p.len());
        let start = at.max(WARMUP);
        // a silent part (a muted speed change, decision 0030) has nothing to align
        let audible = p
            .get(start..next)
            .is_some_and(|w| w.iter().any(|v| v.abs() > 1e-3));
        let part_lag = (next.saturating_sub(start) >= spf && audible)
            .then(|| xcorr(&p, &d, start, next, MAX_LAG));
        let part_ok = part_lag.is_none_or(|(l, c)| l.unsigned_abs() <= limit && c > 0.8);
        joins.push(JoinOffset {
            part: j.index,
            at: j.at,
            lag_samples: lag,
            lag_ms: lag as f64 / 48.0,
            corr,
            part_lag_samples: part_lag.map(|(l, _)| l),
            ok: lag.unsigned_abs() <= limit && corr > 0.9 && part_ok,
        });
    }
    let bad: Vec<_> = joins
        .iter()
        .filter(|j| !j.ok)
        .map(|j| (j.part, j.lag_samples, j.part_lag_samples))
        .collect();
    out.push(check(
        "join_av",
        bad.is_empty(),
        format!("{} discontinuous joins, offsets ≤ {limit} samples required; failing (part, lag, part lag): {bad:?}", joins.len()),
    ));
    let drift = (joins.len() >= 2).then(|| {
        let n = joins.len() as f64;
        let mx = (n - 1.0) / 2.0;
        let my = joins.iter().map(|j| j.lag_ms).sum::<f64>() / n;
        let (num, den) = joins.iter().enumerate().fold((0.0, 0.0), |(a, b), (i, j)| {
            let dx = i as f64 - mx;
            (a + dx * (j.lag_ms - my), b + dx * dx)
        });
        if den > 0.0 { num / den } else { 0.0 }
    });
    out.push(check(
        "drift",
        drift.is_none_or(|s| s.abs() <= 0.1),
        format!(
            "slope {:?} ms/join (≤ 0.1)",
            drift.map(|s| (s * 1000.0).round() / 1000.0)
        ),
    ));
    if let LoudnessMode::Static {
        integrated_lufs,
        true_peak_dbtp,
    } = e.loudness
    {
        let m = loudness::measure(decoded, 2, RATE);
        out.push(check(
            "loudness",
            (m.integrated_lufs - integrated_lufs).abs() <= 0.5 && m.true_peak_dbtp <= true_peak_dbtp + 0.1,
            format!(
                "I {:.2} LUFS (target {integrated_lufs} ±0.5), TP {:.2} dBTP (ceiling {true_peak_dbtp} + 0.1 decoder margin)",
                m.integrated_lufs, m.true_peak_dbtp
            ),
        ));
    }
    (out, joins, drift)
}

/// Container checks, then the audio checks when the decoded audio and planned mix are
/// given (the CPU path has neither until Lane H's decoder runs).
#[must_use]
pub fn verify(
    file: &[u8],
    e: &Expect,
    audio_in: Option<(&[f32], &[f32], &AudioPlan)>,
) -> VerifyReport {
    let mut r = VerifyReport {
        checks: container(file, e),
        ..VerifyReport::default()
    };
    if let Some((decoded, planned, plan)) = audio_in {
        let (c, j, d) = audio(decoded, planned, plan, e);
        r.checks.extend(c);
        r.joins = j;
        r.drift_ms_per_join = d;
    }
    r
}
