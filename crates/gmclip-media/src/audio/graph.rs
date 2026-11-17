//! Audio graph v0 (B-11; RenderPlan rules 4 and 7, SPEC §5.6): f32, 48 kHz, stereo.
//!
//! Order (RP rule 7): part gain → part fades → sum → envelopes → (loudness, B-12).
//! - **Slicing** is sample-exact on the output clock: with `S(f) = f · 48000 / fps`, part
//!   `p` contributes source samples `[first_sample, first_sample + S(out1) − S(out0))`
//!   (rule 4), counted after edit-list and priming removal (the decoder's job).
//! - **Gain** multiplies by `10^(dB/20)` rounded to f32 (ffmpeg `volume`'s float path).
//! - **Fades** run only where the plan puts them, which the plan may do only at
//!   discontinuous joins and at the cut's first and last sample (rule 7; refused here
//!   too). Fade-in over `n` samples: sample `i < n` × `shape(i/n)`. Fade-out over the last
//!   `n`: the `j`-th of them × `shape((n − j)/n)`. `linear` is ffmpeg `afade` `tri` (the reference's
//!   144-sample de-click), `raised_cosine` is `afade` `hsin`, `(1 − cos πx)/2` (gmclip's
//!   4 ms default). Gains are computed in f64 and applied as `f32(f64(x) · g)`, as afade.
//! - **Envelopes** (`linear_db`): at output sample `n`, weight
//!   `w = clamp(min((n − s0)/ramp_in, (s1 − n)/ramp_out), 0, 1)`, gain `10^(Σ g·w / 20)`
//!   over all envelopes — exactly the reference's `aeval` program-gain expression.
//!
//! Contiguous joins (same source, adjacent samples) are never faded, so the mix is
//! sample-continuous across them.
//! - **Speed** (`/1`, decision 0030): a sped part's samples are its source's from
//!   `first_sample`, time-stretched to the part's length with the pitch kept
//!   ([`super::stretch`]), or silence when it is muted; gain and fades follow as usual.

/// Output sample rate of every v0 mix.
pub const RATE: u32 = 48_000;
/// Channels of every v0 mix.
pub const CHANNELS: usize = 2;

/// Fade curve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FadeShape {
    /// `afade` `tri`: gain = x.
    Linear,
    /// `afade` `hsin`: gain = (1 − cos πx) / 2.
    RaisedCosine,
}

impl FadeShape {
    #[must_use]
    pub fn gain(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::Linear => x,
            Self::RaisedCosine => (1.0 - (x * std::f64::consts::PI).cos()) / 2.0,
        }
    }
}

/// A de-click fade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fade {
    pub samples: u32,
    pub shape: FadeShape,
}

impl Fade {
    /// The golden cuts' de-click: 144 samples, linear (3 ms).
    pub const LINEAR_3MS: Self = Self {
        samples: 144,
        shape: FadeShape::Linear,
    };
    /// gmclip's default: 4 ms raised cosine.
    pub const DEFAULT: Self = Self {
        samples: 192,
        shape: FadeShape::RaisedCosine,
    };
}

/// One part's audio.
#[derive(Clone, Debug, PartialEq)]
pub struct PartAudio {
    pub id: String,
    pub source: String,
    pub first_sample: u64,
    /// Output samples `[S(out0), S(out1))`.
    pub out: [u64; 2],
    pub gain_db: f64,
    pub fade_in: Option<Fade>,
    pub fade_out: Option<Fade>,
    /// `/1` (decision 0030): played at `num/den` ×, time-stretched (`true`) or silent.
    pub speed: Option<(u64, u64, bool)>,
    /// `/1` (decision 0033): the part's speed with its ramp, and the output samples a
    /// frame, when it ramps; the stretch then follows the ramp's integral.
    pub ramp: Option<(gmclip_plan::types::PartSpeed, f64)>,
    /// `/1` (decision 0051): sources heard under the part's own audio.
    pub under: Vec<Under>,
}

/// A source heard under a part's audio (decision 0051), and how it meets its neighbours
/// at continuous joins, where the part has no fade: from the gain the part before
/// played it at (0 when it did not), and out to silence when the part after drops it.
#[derive(Clone, Debug, PartialEq)]
pub struct Under {
    pub source: String,
    pub first_sample: u64,
    pub gain_db: f64,
    /// Ramps in over [`UNDER_RAMP`] samples from this amplitude.
    pub ramp_from: Option<f64>,
    /// Ramps out to silence over the last [`UNDER_RAMP`] samples.
    pub ramp_out: bool,
}

/// Frames an under source or an overlay is read in at a time (about 1.4 s).
const BLOCK: usize = 1 << 16;

/// An under source's gain change at a continuous join ramps over 10 ms (linear in
/// amplitude), so the join does not click.
pub const UNDER_RAMP: u64 = 480;

impl Under {
    /// Its gain at sample `i` of a part of `len` samples.
    fn gain_at(&self, i: u64, len: u64) -> f64 {
        let g = 10f64.powf(self.gain_db / 20.0);
        let r = UNDER_RAMP as f64;
        let mut x = match self.ramp_from {
            Some(a) if i < UNDER_RAMP => a + (g - a) * (i as f64 / r),
            _ => g,
        };
        if self.ramp_out && i + UNDER_RAMP >= len {
            x *= ((len - i) as f64 / r).min(1.0);
        }
        x
    }
}

/// Music or a sound effect over the cut (`/1`, decision 0051).
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub id: String,
    pub source: String,
    pub first_sample: u64,
    /// Output samples `[a, b)`.
    pub out: [u64; 2],
    pub gain_db: f64,
    pub fade_in: u64,
    pub fade_out: u64,
    /// `(depth_db, attack, release)` under the plan's speech.
    pub duck: Option<(f64, u64, u64)>,
}

impl Overlay {
    /// Its gain at output sample `n` (inside `out`): gain × linear fades × the duck.
    fn gain_at(&self, n: u64, speech: &[[u64; 2]]) -> f64 {
        let mut g = 10f64.powf(self.gain_db / 20.0);
        let (i, left) = (n - self.out[0], self.out[1] - n);
        if i < self.fade_in {
            g *= i as f64 / self.fade_in as f64;
        }
        if left <= self.fade_out {
            g *= left as f64 / self.fade_out as f64;
        }
        if let Some((depth, attack, release)) = self.duck {
            let w = speech_weight(speech, n, attack, release);
            if w > 0.0 {
                g *= 10f64.powf(depth * w / 20.0);
            }
        }
        g
    }
}

/// How far into a duck output sample `n` is: 1 inside a speech range, rising linearly
/// over the `attack` samples before one and falling over the `release` samples after
/// one (the larger where they meet). `speech` is in order and apart.
#[must_use]
pub fn speech_weight(speech: &[[u64; 2]], n: u64, attack: u64, release: u64) -> f64 {
    let k = speech.partition_point(|r| r[1] <= n);
    let next = speech.get(k).map_or(0.0, |r| {
        if r[0] <= n {
            1.0
        } else {
            1.0 - ((r[0] - n) as f64 / attack.max(1) as f64).min(1.0)
        }
    });
    let prev = k
        .checked_sub(1)
        .and_then(|j| speech.get(j))
        .map_or(0.0, |r| {
            1.0 - ((n + 1 - r[1]) as f64 / release.max(1) as f64).min(1.0)
        });
    next.max(prev)
}

impl PartAudio {
    #[must_use]
    pub fn len(&self) -> u64 {
        self.out[1] - self.out[0]
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A linear-dB gain envelope on the output sample clock.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub id: String,
    pub out: [u64; 2],
    pub gain_db: f64,
    pub ramp_in: u64,
    pub ramp_out: u64,
}

/// Loudness mode (RP rule 8).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoudnessMode {
    AsRecorded,
    Static {
        integrated_lufs: f64,
        true_peak_dbtp: f64,
    },
}

/// The audio half of a RenderPlan.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioPlan {
    /// Total output samples, `S(output.frames)`.
    pub total: u64,
    pub parts: Vec<PartAudio>,
    pub envelopes: Vec<Envelope>,
    /// Music and sound effects (decision 0051).
    pub overlays: Vec<Overlay>,
    /// Where the voice speaks, in output samples, in order and apart (decision 0051).
    pub speech: Vec<[u64; 2]>,
    pub loudness: LoudnessMode,
    /// Each time-stretched part's whole output, made once (decision 0030).
    pub stretched: StretchCache,
}

/// Stretched parts by part index: computed from the part's start, whole, the first time a
/// mix touches it, so a mix in chunks equals the whole mix. Not part of the plan's value
/// (clones start empty, every cache compares equal).
#[derive(Default)]
pub struct StretchCache(
    std::sync::Mutex<std::collections::HashMap<usize, std::sync::Arc<Vec<f32>>>>,
);

impl Clone for StretchCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for StretchCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for StretchCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StretchCache")
    }
}

/// The longest part that is time-stretched in memory (15 min of output).
pub const MAX_STRETCH_FRAMES: u64 = 48_000 * 60 * 15;

/// A join between two consecutive parts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Join {
    /// Index of the part after the join.
    pub index: usize,
    /// Output sample of the join.
    pub at: u64,
    /// Same source, adjacent samples (RP rule 7).
    pub continuous: bool,
}

pub use gmclip_plan::PlanError;

fn err(path: impl Into<String>, reason: impl Into<String>) -> PlanError {
    PlanError::invalid(path, reason)
}

/// Errors while mixing.
#[derive(Debug, thiserror::Error)]
pub enum MixError {
    #[error("part {part}: source {source_id} read failed: {reason}")]
    Source {
        part: String,
        source_id: String,
        reason: String,
    },
    #[error("plan too large to mix in memory ({0} samples)")]
    TooLarge(u64),
}

/// Decoded PCM by source: 48 kHz, interleaved stereo, sample index after edit-list and
/// priming removal (Lane H's decoders; WAV slices in tests).
pub trait PcmSource {
    /// Fills `out` (interleaved stereo, `out.len() / 2` frames) from `source`'s samples
    /// starting at `first_sample`.
    fn read(&mut self, source: &str, first_sample: u64, out: &mut [f32]) -> Result<(), String>;
}

fn fade(f: Option<&gmclip_plan::types::Fade>, path: String) -> Result<Option<Fade>, PlanError> {
    let Some(f) = f else { return Ok(None) };
    let samples = u32::try_from(f.samples)
        .ok()
        .filter(|&s| s >= 1)
        .ok_or_else(|| err(format!("{path}/samples"), "must be 1..2^32"))?;
    let shape = match f.shape {
        gmclip_plan::types::FadeShape::Linear => FadeShape::Linear,
        gmclip_plan::types::FadeShape::RaisedCosine => FadeShape::RaisedCosine,
    };
    Ok(Some(Fade { samples, shape }))
}

impl AudioPlan {
    /// The audio half of a RenderPlan v0 (`output.fps/frames`, `parts[].out/audio`,
    /// `audio.envelopes/loudness`). Give it a plan from `gmclip_plan::parse`, which has
    /// already enforced RP rules 2, 4 and 7; the checks the graph relies on are repeated
    /// here so a plan built in code cannot slip past.
    pub fn from_plan(plan: &gmclip_plan::Plan) -> Result<Self, PlanError> {
        let fps = plan.output.fps;
        if !fps.is_valid() {
            return Err(err(
                "/output/fps",
                "an integer rate 24, 25, 30, 48, 50, 60, or 24000, 30000, 60000 over 1001",
            ));
        }
        // S(f): exact for every integer rate, rounding down at an NTSC one (rule 4)
        let s_at = |f: u64| gmclip_plan::validate::out_samples(f, fps);
        let frames = plan.output.frames;
        let total = s_at(frames)
            .filter(|t| *t <= u64::MAX / 2)
            .ok_or_else(|| err("/output/frames", "too large"))?;
        let mut parts = Vec::with_capacity(plan.parts.len());
        let mut cursor = 0u64;
        for (i, p) in plan.parts.iter().enumerate() {
            let base = format!("/parts/{i}");
            let (a, b) = (p.out.start(), p.out.end());
            if a != cursor || b <= a || b > frames {
                return Err(err(
                    format!("{base}/out"),
                    "parts must tile [0, frames) in order (rule 2)",
                ));
            }
            cursor = b;
            let gain_db = p.audio.gain_db.unwrap_or(0.0);
            if !(-30.0..=12.0).contains(&gain_db) {
                return Err(err(
                    format!("{base}/audio/gain_db"),
                    "a number in [-30, 12]",
                ));
            }
            parts.push(PartAudio {
                id: p.id.as_str().to_owned(),
                source: p.audio.source.as_str().to_owned(),
                first_sample: p.audio.first_sample,
                // b ≤ frames and S(frames) fits (checked above), so these cannot fail.
                out: [s_at(a).unwrap_or(total), s_at(b).unwrap_or(total)],
                gain_db,
                fade_in: fade(p.audio.fade_in(), format!("{base}/audio/fade_in"))?,
                fade_out: fade(p.audio.fade_out(), format!("{base}/audio/fade_out"))?,
                speed: match p.speed {
                    Some(sp) => {
                        let (n, d) = (u128::from(sp.num), u128::from(sp.den));
                        if (n == d && sp.ramp.is_none()) || n * 4 < d || n > 4 * d || d == 0 {
                            return Err(err(format!("{base}/speed"), "num/den in [1/4, 4], not 1"));
                        }
                        Some((sp.num, sp.den, sp.stretches()))
                    }
                    None => None,
                },
                ramp: p.speed.filter(|sp| sp.ramp.is_some()).map(|sp| {
                    let (n, d) = fps.rate();
                    (sp, 48_000.0 * d as f64 / n as f64)
                }),
                under: Vec::new(),
            });
            for (j, u) in p.audio.under.iter().flatten().enumerate() {
                let at = format!("{base}/audio/under/{j}");
                if p.speed.is_some() {
                    return Err(err(
                        at,
                        "a part that changes speed hears only its own audio",
                    ));
                }
                if !(-60.0..=12.0).contains(&u.gain_db) {
                    return Err(err(format!("{at}/gain_db"), "a number in [-60, 12]"));
                }
                if let Some(last) = parts.last_mut() {
                    last.under.push(Under {
                        source: u.source.as_str().to_owned(),
                        first_sample: u.first_sample,
                        gain_db: u.gain_db,
                        ramp_from: None,
                        ramp_out: false,
                    });
                }
            }
        }
        if cursor != frames {
            return Err(err(
                "/parts",
                format!("parts end at frame {cursor}, output.frames is {frames}"),
            ));
        }
        let mut envelopes = Vec::new();
        // The validator accepts envelopes in any order; only overlap is refused.
        let mut order: Vec<usize> =
            (0..plan.audio.envelopes.as_ref().map_or(0, Vec::len)).collect();
        let envs = plan.audio.envelopes.as_deref().unwrap_or(&[]);
        order.sort_by_key(|&i| envs.get(i).map_or(0, |e| e.out_samples.start()));
        let mut prev_end = 0u64;
        for i in order {
            let Some(e) = envs.get(i) else { continue };
            let base = format!("/audio/envelopes/{i}");
            let (s0, s1) = (e.out_samples.start(), e.out_samples.end());
            if s1 <= s0 || s1 > total || s0 < prev_end {
                return Err(err(
                    format!("{base}/out_samples"),
                    "inside [0, total] and not overlapping another envelope",
                ));
            }
            prev_end = s1;
            if e.ramp_in_samples == 0 || e.ramp_out_samples == 0 {
                return Err(err(base, "ramps must be ≥ 1 sample"));
            }
            if !(-30.0..=24.0).contains(&e.gain_db) {
                return Err(err(format!("{base}/gain_db"), "a number in [-30, 24]"));
            }
            envelopes.push(Envelope {
                id: e.id.as_str().to_owned(),
                out: [s0, s1],
                gain_db: e.gain_db,
                ramp_in: e.ramp_in_samples,
                ramp_out: e.ramp_out_samples,
            });
        }
        let loudness = match plan.audio.loudness {
            gmclip_plan::types::Loudness::AsRecorded => LoudnessMode::AsRecorded,
            gmclip_plan::types::Loudness::Static {
                integrated_lufs,
                true_peak_dbtp,
            } => LoudnessMode::Static {
                integrated_lufs,
                true_peak_dbtp,
            },
        };
        let mut overlays = Vec::new();
        for (i, o) in plan.audio.overlays.iter().flatten().enumerate() {
            let base = format!("/audio/overlays/{i}");
            let (a, b) = (o.out_samples.start(), o.out_samples.end());
            let (fi, fo) = (
                o.fade_in_samples.unwrap_or(0),
                o.fade_out_samples.unwrap_or(0),
            );
            if b <= a || b > total || fi.saturating_add(fo) > b - a {
                return Err(err(
                    format!("{base}/out_samples"),
                    "inside [0, total] with its fades inside it",
                ));
            }
            if !(-60.0..=12.0).contains(&o.gain_db) {
                return Err(err(format!("{base}/gain_db"), "a number in [-60, 12]"));
            }
            overlays.push(Overlay {
                id: o.id.as_str().to_owned(),
                source: o.source.as_str().to_owned(),
                first_sample: o.first_sample,
                out: [a, b],
                gain_db: o.gain_db,
                fade_in: fi,
                fade_out: fo,
                duck: o
                    .duck
                    .map(|d| (d.depth_db, d.attack_samples, d.release_samples)),
            });
        }
        let mut speech: Vec<[u64; 2]> = Vec::new();
        for (i, r) in plan.audio.speech.iter().flatten().enumerate() {
            let (a, b) = (r.start(), r.end());
            if b <= a || b > total || speech.last().is_some_and(|l| a < l[1]) {
                return Err(err(
                    format!("/audio/speech/{i}"),
                    "in order, apart, inside [0, total]",
                ));
            }
            speech.push([a, b]);
        }
        let mut plan = Self {
            total,
            parts,
            envelopes,
            overlays,
            speech,
            loudness,
            stretched: StretchCache::default(),
        };
        // under sources across continuous joins: carry on, change gain, or come and go
        // with a ramp (where a discontinuous join's fades already shape the sum)
        for j in plan.joins().into_iter().filter(|j| j.continuous) {
            let (head, tail) = plan.parts.split_at_mut(j.index);
            let (Some(a), Some(b)) = (head.last_mut(), tail.first_mut()) else {
                continue;
            };
            let carried = |u: &Under, from: &PartAudio| {
                from.under
                    .iter()
                    .find(|x| x.source == u.source && x.first_sample + from.len() == u.first_sample)
                    .map(|x| x.gain_db)
            };
            for u in &mut b.under {
                let before = carried(u, a);
                if before != Some(u.gain_db) {
                    u.ramp_from = Some(before.map_or(0.0, |db| 10f64.powf(db / 20.0)));
                }
            }
            for u in &mut a.under {
                let len = a.out[1] - a.out[0];
                u.ramp_out = !b
                    .under
                    .iter()
                    .any(|x| x.source == u.source && u.first_sample + len == x.first_sample);
            }
        }
        for j in plan.joins() {
            if j.continuous {
                let before = j.index - 1;
                if plan.parts.get(before).and_then(|p| p.fade_out).is_some() {
                    return Err(err(
                        format!("/parts/{before}/audio/fade_out"),
                        "fade at a continuous join (rule 7)",
                    ));
                }
                if plan.parts.get(j.index).and_then(|p| p.fade_in).is_some() {
                    return Err(err(
                        format!("/parts/{}/audio/fade_in", j.index),
                        "fade at a continuous join (rule 7)",
                    ));
                }
            }
        }
        Ok(plan)
    }

    /// Every join and whether it is continuous (RP rule 7).
    #[must_use]
    pub fn joins(&self) -> Vec<Join> {
        self.parts
            .windows(2)
            .enumerate()
            .filter_map(|(i, w)| match w {
                [a, b] => Some(Join {
                    index: i + 1,
                    at: b.out[0],
                    // a sped part is never continuous with its neighbours (decision 0030)
                    continuous: a.speed.is_none()
                        && b.speed.is_none()
                        && a.source == b.source
                        && a.first_sample.checked_add(a.len()) == Some(b.first_sample),
                }),
                _ => None,
            })
            .collect()
    }

    /// Mixes the plan: interleaved stereo f32, `total` frames, before loudness.
    pub fn mix(&self, src: &mut dyn PcmSource) -> Result<Vec<f32>, MixError> {
        let len = usize::try_from(self.total)
            .ok()
            .and_then(|t| t.checked_mul(CHANNELS))
            .ok_or(MixError::TooLarge(self.total))?;
        if self.total > MAX_MIX_FRAMES {
            return Err(MixError::TooLarge(self.total));
        }
        let mut out: Vec<f32> = Vec::new();
        out.try_reserve_exact(len)
            .map_err(|_| MixError::TooLarge(self.total))?;
        out.resize(len, 0.0);
        // one path for the whole mix and its slices: the export's mix is playback's
        self.mix_range(src, 0, &mut out)?;
        Ok(out)
    }

    /// Mixes output samples `[from, from + out.len() / 2)` into `out` (interleaved stereo),
    /// bit-identical to the same slice of [`AudioPlan::mix`] (playback reads the plan in
    /// chunks; the export mixes it whole). Samples past `total` are silence.
    pub fn mix_range(
        &self,
        src: &mut dyn PcmSource,
        from: u64,
        out: &mut [f32],
    ) -> Result<(), MixError> {
        out.fill(0.0);
        let frames = (out.len() / CHANNELS) as u64;
        let to = from.saturating_add(frames).min(self.total);
        let mut scratch: Vec<f32> = Vec::new();
        for (pi, p) in self.parts.iter().enumerate() {
            let (a, b) = (p.out[0].max(from), p.out[1].min(to));
            if a >= b {
                continue;
            }
            let i0 = usize::try_from(a - from).map_err(|_| MixError::TooLarge(a))? * CHANNELS;
            let i1 = usize::try_from(b - from).map_err(|_| MixError::TooLarge(b))? * CHANNELS;
            let Some(buf) = out.get_mut(i0..i1) else {
                return Err(MixError::TooLarge(b));
            };
            let k0 = a - p.out[0];
            if p.speed.is_some() {
                let whole = self.stretched_part(pi, p, src)?;
                let s0 = usize::try_from(k0).map_err(|_| MixError::TooLarge(k0))? * CHANNELS;
                buf.copy_from_slice(&whole[s0..s0 + buf.len()]);
            } else {
                src.read(&p.source, p.first_sample + k0, buf)
                    .map_err(|reason| MixError::Source {
                        part: p.id.clone(),
                        source_id: p.source.clone(),
                        reason,
                    })?;
            }
            part_gain(p, buf);
            for u in &p.under {
                let len = p.len();
                // in blocks, so a long part needs no second whole-length buffer
                for (bi, block) in buf.chunks_mut(BLOCK * CHANNELS).enumerate() {
                    let k = k0 + (bi * BLOCK) as u64;
                    scratch.resize(block.len(), 0.0);
                    src.read(&u.source, u.first_sample + k, &mut scratch)
                        .map_err(|reason| MixError::Source {
                            part: p.id.clone(),
                            source_id: u.source.clone(),
                            reason,
                        })?;
                    for (j, (o, x)) in block
                        .chunks_mut(CHANNELS)
                        .zip(scratch.chunks(CHANNELS))
                        .enumerate()
                    {
                        let g = u.gain_at(k + j as u64, len);
                        for (o, x) in o.iter_mut().zip(x) {
                            *o = (f64::from(*o) + f64::from(*x) * g) as f32;
                        }
                    }
                }
            }
            part_fades(p, k0, buf);
        }
        // the sum: music and sound effects over the parts (decision 0051)
        for o in &self.overlays {
            let (a, b) = (o.out[0].max(from), o.out[1].min(to));
            if a >= b {
                continue;
            }
            let i0 = usize::try_from(a - from).map_err(|_| MixError::TooLarge(a))? * CHANNELS;
            let i1 = usize::try_from(b - from).map_err(|_| MixError::TooLarge(b))? * CHANNELS;
            let Some(buf) = out.get_mut(i0..i1) else {
                return Err(MixError::TooLarge(b));
            };
            for (bi, block) in buf.chunks_mut(BLOCK * CHANNELS).enumerate() {
                let n0 = a + (bi * BLOCK) as u64;
                scratch.resize(block.len(), 0.0);
                src.read(&o.source, o.first_sample + (n0 - o.out[0]), &mut scratch)
                    .map_err(|reason| MixError::Source {
                        part: o.id.clone(),
                        source_id: o.source.clone(),
                        reason,
                    })?;
                for (j, (y, x)) in block
                    .chunks_mut(CHANNELS)
                    .zip(scratch.chunks(CHANNELS))
                    .enumerate()
                {
                    let g = o.gain_at(n0 + j as u64, &self.speech);
                    for (y, x) in y.iter_mut().zip(x) {
                        *y = (f64::from(*y) + f64::from(*x) * g) as f32;
                    }
                }
            }
        }
        for e in &self.envelopes {
            let (a, b) = (e.out[0].max(from), e.out[1].min(to));
            for n in a..b {
                let g = 10f64.powf(envelope_term(e, n as f64) / 20.0);
                let i = usize::try_from(n - from).unwrap_or(usize::MAX) * CHANNELS;
                if let Some(frame) = out.get_mut(i..i + CHANNELS) {
                    for s in frame {
                        *s = (f64::from(*s) * g) as f32;
                    }
                }
            }
        }
        Ok(())
    }

    /// A sped part's whole output (before gain and fades): its source samples
    /// time-stretched to its length, or silence when it is muted (decision 0030). Made
    /// once per plan and kept.
    fn stretched_part(
        &self,
        index: usize,
        p: &PartAudio,
        src: &mut dyn PcmSource,
    ) -> Result<std::sync::Arc<Vec<f32>>, MixError> {
        let Some((num, den, play)) = p.speed else {
            return Err(MixError::TooLarge(0));
        };
        if let Some(v) = self
            .stretched
            .0
            .lock()
            .ok()
            .and_then(|m| m.get(&index).cloned())
        {
            return Ok(v);
        }
        let n = p.len();
        if n > MAX_STRETCH_FRAMES {
            return Err(MixError::TooLarge(n));
        }
        let frames = usize::try_from(n).map_err(|_| MixError::TooLarge(n))?;
        let whole = if play {
            let need = match p.ramp {
                Some((sp, spf)) => u128::from(sp.source_samples_at(n, spf)),
                None => (u128::from(n) * u128::from(num)).div_ceil(u128::from(den.max(1))),
            };
            let need =
                usize::try_from(need).map_err(|_| MixError::TooLarge(n))? + super::stretch::MARGIN;
            let mut input = vec![0.0f32; need * CHANNELS];
            src.read(&p.source, p.first_sample, &mut input)
                .map_err(|reason| MixError::Source {
                    part: p.id.clone(),
                    source_id: p.source.clone(),
                    reason,
                })?;
            match p.ramp {
                // the ramp's integral on the sample clock (decision 0033)
                Some((sp, spf)) => {
                    super::stretch::stretch_by(&input, frames, &|s| sp.advance_f64(s / spf) * spf)
                }
                None => super::stretch::stretch(&input, num, den, frames),
            }
        } else {
            vec![0.0f32; frames * CHANNELS]
        };
        let whole = std::sync::Arc::new(whole);
        if let Ok(mut m) = self.stretched.0.lock() {
            m.insert(index, std::sync::Arc::clone(&whole));
        }
        Ok(whole)
    }

    /// Envelope gain in dB at output sample `n` (summed over envelopes).
    #[must_use]
    pub fn envelope_db(&self, n: u64) -> f64 {
        self.envelopes
            .iter()
            .map(|e| envelope_term(e, n as f64))
            .sum()
    }
}

/// One envelope's dB at output sample `n` (the reference's aeval term).
fn envelope_term(e: &Envelope, n: f64) -> f64 {
    let w =
        ((n - e.out[0] as f64) / e.ramp_in as f64).min((e.out[1] as f64 - n) / e.ramp_out as f64);
    e.gain_db * w.clamp(0.0, 1.0)
}

/// Longest mix held in memory: 2 hours (the reference cap is 15 minutes).
pub const MAX_MIX_FRAMES: u64 = 48_000 * 60 * 60 * 2;

/// Part gain, in place on a slice of the part's own audio.
fn part_gain(p: &PartAudio, buf: &mut [f32]) {
    if p.gain_db != 0.0 {
        let g = 10f64.powf(p.gain_db / 20.0) as f32;
        buf.iter_mut().for_each(|s| *s *= g);
    }
}

/// Part fades, in place on a slice of the part's sum starting `k0` samples into it.
fn part_fades(p: &PartAudio, k0: u64, buf: &mut [f32]) {
    let frames = p.len();
    for (j, frame) in buf.chunks_mut(CHANNELS).enumerate() {
        let i = k0 + j as u64; // sample index within the part
        if let Some(f) = p.fade_in {
            let n = u64::from(f.samples);
            if i < n {
                let g = f.shape.gain(i as f64 / n as f64);
                frame
                    .iter_mut()
                    .for_each(|s| *s = (f64::from(*s) * g) as f32);
            }
        }
        if let Some(f) = p.fade_out {
            let n = u64::from(f.samples);
            let start = frames.saturating_sub(n);
            let skipped = n.saturating_sub(frames);
            if i >= start {
                let g = f.shape.gain((n - (i - start + skipped)) as f64 / n as f64);
                frame
                    .iter_mut()
                    .for_each(|s| *s = (f64::from(*s) * g) as f32);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A source whose sample k (both channels) is a deterministic noise value.
    struct Noise;
    fn noise(k: u64) -> f32 {
        let x = k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40;
        (x as f32 / (1u64 << 24) as f32) - 0.5
    }
    impl PcmSource for Noise {
        fn read(&mut self, source: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
            let off = if source == "b" { 1_000_000 } else { 0 };
            for (i, f) in out.chunks_mut(2).enumerate() {
                let v = noise(off + first + i as u64);
                f[0] = v;
                f[1] = -v;
            }
            Ok(())
        }
    }

    fn plan(parts: Value, envelopes: Value, frames: u64) -> Value {
        json!({ "schema": "gmclip.renderplan/0",
                "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": frames,
                            "video": { "codec": "h264" }, "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2 } },
                "background": { "kind": "colour", "rgb": "#000000" },
                "sources": [ { "id": "a", "path": "/a" }, { "id": "b", "path": "/b" } ],
                "parts": parts,
                "audio": { "envelopes": envelopes, "loudness": { "mode": "as_recorded" } } })
    }

    fn part(out: [u64; 2], source: &str, first: u64, fin: Value, fout: Value) -> Value {
        json!({ "id": format!("p{}", out[0]), "out": out, "picture": { "source": source, "first_frame": 0 },
                "audio": { "source": source, "first_sample": first, "rate": 48000, "fade_in": fin, "fade_out": fout },
                "layers": [] })
    }

    fn parse(v: &Value) -> Result<gmclip_plan::Plan, PlanError> {
        gmclip_plan::parse(&serde_json::to_vec(v).unwrap())
    }

    fn audio_plan(v: &Value) -> AudioPlan {
        AudioPlan::from_plan(&parse(v).unwrap()).unwrap()
    }

    const LIN: fn() -> Value = || json!({ "samples": 144, "shape": "linear" });

    /// Mixes `ap` in uneven chunks and checks the result is the whole mix, bit for bit.
    fn chunks_equal_whole(ap: &AudioPlan) -> Vec<f32> {
        let whole = ap.mix(&mut Noise).unwrap();
        for sizes in [[1usize, 37, 1024, 4799], [480, 479, 481, 16_000]] {
            let mut got = Vec::new();
            let mut at = 0u64;
            let mut k = 0;
            while at < ap.total {
                let n = sizes[k % sizes.len()];
                k += 1;
                let mut buf = vec![1.0f32; n * CHANNELS];
                ap.mix_range(&mut Noise, at, &mut buf).unwrap();
                got.extend_from_slice(&buf);
                at += n as u64;
            }
            let first = got.iter().zip(&whole).position(|(a, b)| a != b);
            assert_eq!(first, None, "the chunked mix differs ({sizes:?})");
        }
        whole
    }

    /// Decision 0051: a source under the part's audio sums at its own gain (the part gain
    /// is the voice's alone), the part's fades shape the sum, a gain change at a
    /// continuous join ramps over 10 ms, a source the next part drops ramps out, and the
    /// mix in chunks is the whole mix.
    #[test]
    fn sources_under_a_part_sum_at_their_gain_and_ramp_at_joins() {
        let mut v = plan(
            json!([
                part([0, 10], "a", 0, Value::Null, Value::Null),
                part([10, 20], "a", 16_000, Value::Null, Value::Null),
                part([20, 30], "a", 32_000, Value::Null, LIN())
            ]),
            json!([]),
            30,
        );
        v["schema"] = json!("gmclip.renderplan/1");
        v["parts"][0]["audio"]["gain_db"] = json!(6.0);
        v["parts"][0]["audio"]["under"] =
            json!([{"source": "b", "first_sample": 500, "gain_db": -12.0}]);
        v["parts"][1]["audio"]["under"] =
            json!([{"source": "b", "first_sample": 16_500, "gain_db": -6.0}]);
        let ap = audio_plan(&v);
        assert!(ap.joins().iter().all(|j| j.continuous));
        let u1 = &ap.parts[1].under[0];
        assert_eq!(u1.ramp_from, Some(10f64.powf(-12.0 / 20.0)));
        assert!(!ap.parts[0].under[0].ramp_out && ap.parts[1].under[0].ramp_out);
        let y = chunks_equal_whole(&ap);
        let at = |n: usize| y[2 * n];
        let b = |k: u64| f64::from(noise(1_000_000 + k));
        let voice = |n: u64, db: f64| f64::from(noise(n) * 10f64.powf(db / 20.0) as f32);
        // inside part 0: voice +6 dB, b at −12 dB
        let n = 3000u64;
        let want = (voice(n, 6.0) + b(500 + n) * 10f64.powf(-0.6)) as f32;
        assert_eq!(at(n as usize), want);
        // 240 samples into part 1: halfway from −12 to −6 dB in amplitude
        let (g0, g1) = (10f64.powf(-0.6), 10f64.powf(-0.3));
        let n = 16_000 + 240u64;
        let want = (voice(n, 0.0) + b(500 + n) * (g0 + (g1 - g0) * 0.5)) as f32;
        assert_eq!(at(n as usize), want);
        // part 1's last sample: b ramps out to 1/480 of −6 dB; part 2 has the voice alone
        let n = 32_000 - 1u64;
        let want = (voice(n, 0.0) + b(500 + n) * g1 / 480.0) as f32;
        assert_eq!(at(n as usize), want);
        assert_eq!(at(40_000), noise(40_000));
        // without the under sources the parts are the voice alone
        let mut bare = v.clone();
        for i in 0..2 {
            bare["parts"][i]["audio"]
                .as_object_mut()
                .unwrap()
                .remove("under");
        }
        let plain = audio_plan(&bare).mix(&mut Noise).unwrap();
        assert_ne!(plain[2 * 3000], at(3000));
        assert_eq!(plain[2 * 40_000], at(40_000));
    }

    /// Decision 0051: an overlay reads its source from `first_sample` over its output
    /// span, fades in and out linearly, and ducks under speech with its attack before and
    /// release after; the mix in chunks is the whole mix.
    #[test]
    fn overlays_fade_and_duck_under_speech() {
        let mut v = plan(
            json!([part([0, 60], "a", 0, Value::Null, Value::Null)]),
            json!([]),
            60,
        );
        v["schema"] = json!("gmclip.renderplan/1");
        v["audio"]["overlays"] = json!([{"id": "m", "source": "b", "first_sample": 7,
            "out_samples": [4800, 90_000], "gain_db": -10.0, "fade_in_samples": 960,
            "fade_out_samples": 4800,
            "duck": {"depth_db": -12.0, "attack_samples": 2400, "release_samples": 9600}}]);
        v["audio"]["speech"] = json!([[20_000, 30_000], [50_000, 60_000]]);
        let ap = audio_plan(&v);
        let y = chunks_equal_whole(&ap);
        let m = |n: u64, g: f64| {
            (f64::from(noise(n)) + f64::from(noise(1_000_000 + 7 + n - 4800)) * g) as f32
        };
        let base = 10f64.powf(-0.5);
        assert_eq!(y[2 * 4000], noise(4000), "before the overlay");
        assert_eq!(y[2 * 4800], m(4800, 0.0), "fades in from silence");
        assert_eq!(y[2 * 5280], m(5280, base * 0.5));
        assert_eq!(y[2 * 10_000], m(10_000, base), "full, far from speech");
        assert_eq!(y[2 * 25_000], m(25_000, base * 10f64.powf(-0.6)), "ducked");
        // half the attack before speech: half the depth
        assert_eq!(y[2 * 18_800], m(18_800, base * 10f64.powf(-0.3)));
        assert_eq!(speech_weight(&ap.speech, 30_000 + 4799, 2400, 9600), 0.5);
        assert_eq!(speech_weight(&ap.speech, 45_000, 2400, 9600), 0.0);
        assert_eq!(
            speech_weight(&ap.speech, 49_000, 2400, 9600),
            1.0 - 1000.0 / 2400.0
        );
        assert_eq!(y[2 * 89_999], m(89_999, base / 4800.0), "fades out");
        assert_eq!(y[2 * 90_000], noise(90_000), "after it");
    }

    /// Decision 0030: a sped part is stretched once, so a mix in chunks equals the whole
    /// mix; a muted one is silent; a join touching one is not continuous.
    #[test]
    fn sped_parts_stretch_or_mute_and_chunk_exactly() {
        let mut v = plan(
            json!([
                part([0, 30], "a", 0, Value::Null, Value::Null),
                part([30, 60], "a", 48_000, Value::Null, Value::Null),
                part([60, 90], "a", 96_000, Value::Null, Value::Null)
            ]),
            json!([]),
            90,
        );
        v["schema"] = json!("gmclip.renderplan/1");
        v["parts"][1]["speed"] = json!({"num": 2, "den": 1});
        v["parts"][2]["speed"] = json!({"num": 1, "den": 2, "audio": "mute"});
        let ap = audio_plan(&v);
        assert_eq!(ap.parts[1].speed, Some((2, 1, true)));
        assert!(ap.joins().iter().all(|j| !j.continuous));
        let whole = ap.mix(&mut Noise).unwrap();
        let mut chunked = vec![0.0f32; whole.len()];
        let mut at = 0u64;
        for n in [1000usize, 777, 4096, 31, 20_000, 100_000] {
            let a = (at as usize) * 2;
            if a >= chunked.len() {
                break;
            }
            let b = (a + n * 2).min(chunked.len());
            ap.mix_range(&mut Noise, at, &mut chunked[a..b]).unwrap();
            at += ((b - a) / 2) as u64;
        }
        assert_eq!(whole, chunked);
        // the 2× part is not the source read straight (it is stretched), the muted one silent
        let s1 = 48_000 * 2;
        let differ = (0..48_000)
            .filter(|&i| (whole[s1 + 2 * i] - noise(48_000 + i as u64)).abs() > 1e-6)
            .count();
        assert!(differ > 10_000, "{differ}");
        assert!(whole[s1..s1 + 96_000].iter().any(|x| *x != 0.0));
        assert!(whole[96_000 * 2..].iter().all(|x| *x == 0.0));
    }

    /// Decision 0033: a ramped part is stretched along its integral, once, so the chunked
    /// mix equals the whole one; it reads exactly the source its ramp reaches.
    #[test]
    fn ramped_parts_stretch_along_their_integral_and_chunk_exactly() {
        let mut v = plan(
            json!([
                part([0, 30], "a", 0, Value::Null, Value::Null),
                part([30, 90], "a", 48_000, Value::Null, Value::Null)
            ]),
            json!([]),
            90,
        );
        v["schema"] = json!("gmclip.renderplan/1");
        v["parts"][1]["speed"] = json!({"num": 1, "den": 1,
            "ramp": {"to": {"num": 2, "den": 1}, "from": 10, "frames": 30, "curve": "ease_in_out"}});
        let ap = audio_plan(&v);
        assert_eq!(ap.parts[1].speed, Some((1, 1, true)));
        let (sp, spf) = ap.parts[1].ramp.unwrap();
        assert_eq!(spf, 1600.0);
        // 10 frames at 1×, 30 at an average 1.5×, 20 at 2×: 95 frames of source
        assert_eq!(sp.source_samples_at(60 * 1600, spf), 95 * 1600);
        assert!(ap.joins().iter().all(|j| !j.continuous));
        let whole = ap.mix(&mut Noise).unwrap();
        let mut chunked = vec![0.0f32; whole.len()];
        let mut at = 0u64;
        for n in [999usize, 4096, 31, 20_000, 100_000, 50_000] {
            let a = (at as usize) * 2;
            if a >= chunked.len() {
                break;
            }
            let b = (a + n * 2).min(chunked.len());
            ap.mix_range(&mut Noise, at, &mut chunked[a..b]).unwrap();
            at += ((b - a) / 2) as u64;
        }
        assert!(at >= 144_000);
        let first = whole.iter().zip(&chunked).position(|(a, b)| a != b);
        assert_eq!(first, None, "the chunked mix differs");
        assert!(whole[96_000..].iter().any(|x| *x != 0.0));
    }

    #[test]
    fn mix_range_in_chunks_equals_the_whole_mix() {
        let p = plan(
            json!([
                part([0, 3], "a", 1000, Value::Null, LIN()),
                part([3, 7], "b", 5, LIN(), Value::Null),
                part(
                    [7, 10],
                    "b",
                    5 + 6400,
                    Value::Null,
                    json!({ "samples": 3000, "shape": "raised_cosine" })
                )
            ]),
            json!([{ "id": "e", "out_samples": [2000, 12000], "gain_db": 6.0, "ramp_in_samples": 500, "ramp_out_samples": 700, "curve": "linear_db" }]),
            10,
        );
        let ap = audio_plan(&p);
        let whole = ap.mix(&mut Noise).unwrap();
        for chunk in [1u64, 37, 1024, 4799, 16000] {
            let mut got = Vec::new();
            let mut at = 0;
            while at < ap.total + 100 {
                let mut buf = vec![1.0f32; chunk as usize * CHANNELS];
                ap.mix_range(&mut Noise, at, &mut buf).unwrap();
                got.extend_from_slice(&buf);
                at += chunk;
            }
            assert_eq!(&got[..whole.len()], &whole[..], "chunk {chunk}");
            assert!(
                got[whole.len()..].iter().all(|x| *x == 0.0),
                "silence past the end"
            );
        }
    }

    #[test]
    fn slicing_is_sample_exact_and_telescopes() {
        let p = plan(
            json!([
                part([0, 3], "a", 1000, Value::Null, Value::Null),
                part([3, 10], "b", 5, Value::Null, Value::Null)
            ]),
            json!([]),
            10,
        );
        let ap = audio_plan(&p);
        assert_eq!(ap.total, 16_000);
        assert_eq!(
            (ap.parts[0].out, ap.parts[1].out),
            ([0, 4800], [4800, 16_000])
        );
        let y = ap.mix(&mut Noise).unwrap();
        for n in [0usize, 4799] {
            assert_eq!(y[2 * n], noise(1000 + n as u64));
        }
        assert_eq!(y[2 * 4800], noise(1_000_000 + 5));
        assert_eq!(y[2 * 15_999 + 1], -noise(1_000_000 + 5 + 11_199));
    }

    #[test]
    fn continuous_joins_are_sample_continuous() {
        // One span split three ways (continuous), vs the same span as one part.
        let split = plan(
            json!([
                part([0, 7], "a", 800, LIN(), Value::Null),
                part([7, 8], "a", 800 + 7 * 1600, Value::Null, Value::Null),
                part([8, 20], "a", 800 + 8 * 1600, Value::Null, LIN())
            ]),
            json!([]),
            20,
        );
        let whole = plan(
            json!([part([0, 20], "a", 800, LIN(), LIN())]),
            json!([]),
            20,
        );
        let a = audio_plan(&split);
        assert!(a.joins().iter().all(|j| j.continuous));
        assert_eq!(
            a.mix(&mut Noise).unwrap(),
            audio_plan(&whole).mix(&mut Noise).unwrap()
        );
    }

    #[test]
    fn fades_at_continuous_joins_are_refused() {
        // The plan validator refuses them, and so does the graph for a plan built in code.
        let p = plan(
            json!([
                part([0, 7], "a", 0, Value::Null, Value::Null),
                part([7, 8], "a", 7 * 1600, Value::Null, Value::Null)
            ]),
            json!([]),
            8,
        );
        let mut bad = p.clone();
        bad["parts"][0]["audio"]["fade_out"] = LIN();
        assert!(parse(&bad).is_err(), "validator refuses");
        let mut typed = parse(&p).unwrap();
        let f = gmclip_plan::types::Fade {
            samples: 144,
            shape: gmclip_plan::types::FadeShape::Linear,
        };
        typed.parts[0].audio.fade_out = Some(Some(f));
        let e = AudioPlan::from_plan(&typed).unwrap_err();
        assert!(e.to_string().contains("/parts/0/audio/fade_out"), "{e}");
        let mut typed = parse(&p).unwrap();
        typed.parts[1].audio.fade_in = Some(Some(f));
        let e = AudioPlan::from_plan(&typed).unwrap_err();
        assert!(e.to_string().contains("/parts/1/audio/fade_in"), "{e}");
    }

    #[test]
    fn fade_curves_match_afade() {
        let p = plan(
            json!([part(
                [0, 1],
                "a",
                0,
                LIN(),
                json!({ "samples": 192, "shape": "raised_cosine" })
            )]),
            json!([]),
            1,
        );
        let ap = audio_plan(&p);
        let y = ap.mix(&mut Noise).unwrap();
        assert_eq!(y[0], 0.0, "fade-in starts at 0");
        let x = noise(72);
        assert_eq!(y[2 * 72], (f64::from(x) * 0.5) as f32);
        assert_eq!(y[2 * 144], noise(144), "fade-in over after 144 samples");
        // Fade-out: the j-th of the last 192 samples × hsin((192 − j)/192).
        let j = 96usize;
        let k = 1600 - 192 + j;
        assert_eq!(y[2 * k], (f64::from(noise(k as u64)) * 0.5) as f32);
        let last = (1.0 - (std::f64::consts::PI / 192.0).cos()) / 2.0;
        assert_eq!(
            y[2 * 1599],
            (f64::from(noise(1599)) * last) as f32,
            "last sample keeps 1/n of the curve"
        );
    }

    #[test]
    fn gain_and_envelopes_follow_rt() {
        let mut p = plan(
            json!([part([0, 60], "a", 0, Value::Null, Value::Null)]),
            json!([
                { "id": "e", "out_samples": [1600, 86_400], "gain_db": 10, "ramp_in_samples": 3840, "ramp_out_samples": 5760, "curve": "linear_db" }
            ]),
            60,
        );
        p["parts"][0]["audio"]["gain_db"] = json!(-6.0);
        let ap = audio_plan(&p);
        assert_eq!(ap.envelope_db(1600), 0.0);
        assert_eq!(ap.envelope_db(1600 + 1920), 5.0);
        assert_eq!(ap.envelope_db(50_000), 10.0);
        assert_eq!(ap.envelope_db(86_400 - 2880), 5.0);
        assert_eq!(ap.envelope_db(86_400), 0.0);
        let y = ap.mix(&mut Noise).unwrap();
        let g = 10f64.powf(-6.0 / 20.0) as f32;
        assert_eq!(y[0], noise(0) * g);
        let n = 50_000usize;
        assert_eq!(
            y[2 * n],
            (f64::from(noise(n as u64) * g) * 10f64.powf(0.5)) as f32
        );
    }

    #[test]
    fn default_fade_is_4ms_raised_cosine() {
        assert_eq!(
            Fade::DEFAULT,
            Fade {
                samples: 4 * 48,
                shape: FadeShape::RaisedCosine
            }
        );
        assert_eq!(
            Fade::LINEAR_3MS,
            Fade {
                samples: 144,
                shape: FadeShape::Linear
            }
        );
    }

    #[test]
    fn envelopes_in_any_order() {
        let p = plan(
            json!([part([0, 60], "a", 0, Value::Null, Value::Null)]),
            json!([
                { "id": "late", "out_samples": [50_000, 90_000], "gain_db": 3, "ramp_in_samples": 10, "ramp_out_samples": 10, "curve": "linear_db" },
                { "id": "early", "out_samples": [0, 40_000], "gain_db": -3, "ramp_in_samples": 10, "ramp_out_samples": 10, "curve": "linear_db" }
            ]),
            60,
        );
        let ap = audio_plan(&p);
        assert_eq!(ap.envelope_db(20_000), -3.0);
        assert_eq!(ap.envelope_db(70_000), 3.0);
    }

    #[test]
    fn huge_plans_are_refused_not_aborted() {
        let mut p = parse(&plan(
            json!([part([0, 2], "a", 0, Value::Null, Value::Null)]),
            json!([]),
            2,
        ))
        .unwrap();
        p.output.frames = 1 << 40;
        p.parts[0].out = gmclip_plan::scalars::Range([0, 1 << 40]);
        let ap = AudioPlan::from_plan(&p).unwrap();
        assert!(matches!(ap.mix(&mut Noise), Err(MixError::TooLarge(_))));
        p.output.frames = u64::MAX / 100;
        p.parts[0].out = gmclip_plan::scalars::Range([0, u64::MAX / 100]);
        assert!(
            AudioPlan::from_plan(&p).is_err(),
            "frames × spf overflows: refused"
        );
    }

    #[test]
    fn rejects_broken_plans_built_in_code() {
        let ok = parse(&plan(
            json!([part([0, 2], "a", 0, Value::Null, Value::Null)]),
            json!([]),
            2,
        ))
        .unwrap();
        let msg = |p: &gmclip_plan::Plan| AudioPlan::from_plan(p).unwrap_err().to_string();
        let mut p = ok.clone();
        p.output.frames = 3;
        assert!(msg(&p).contains("/parts"));
        let mut p = ok.clone();
        p.parts[0].out = gmclip_plan::scalars::Range([1, 2]);
        assert!(msg(&p).contains("/parts/0/out"));
        let mut p = ok.clone();
        p.parts[0].audio.gain_db = Some(40.0);
        assert!(msg(&p).contains("/parts/0/audio/gain_db"));
        let mut p = ok;
        p.parts[0].audio.fade_in = Some(Some(gmclip_plan::types::Fade {
            samples: 0,
            shape: gmclip_plan::types::FadeShape::Linear,
        }));
        assert!(msg(&p).contains("/parts/0/audio/fade_in/samples"));
    }
}
