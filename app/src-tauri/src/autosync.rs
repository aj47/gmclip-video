//! "Are these the same recording?": the offset between the file with the voice and the
//! hi-res picture of the same stream, measured instead of typed (coordinator round 2026-10-15).
//!
//! 1. **Sound against sound.** Both files' audio as a 100 Hz loudness envelope (log RMS,
//!    a 1 s moving mean taken off, normalised). Windows of 60 s every 30 s along the voice
//!    file are cross-correlated (FFT) with the whole hi-res envelope; each gives an offset,
//!    a peak and how far the peak stands above the next best (the confidence).
//! 2. **Stretches.** Confident windows that agree (±50 ms) form a stretch; a change of
//!    offset between two stretches is placed by 5 s windows scored at both offsets, and
//!    voice time that matches neither (the hi-res file lost those seconds) is reported.
//! 3. **Picture against picture.** A recording's own sound and picture need not line up
//!    (loc_4fadb756's proxy is 4 frames late after its reconnect), so when both files
//!    have a picture each stretch is settled frame-exactly: small grey frames of both,
//!    compared at the sound's offset ±8 frames; the closest wins when it stands out.
//!
//! Offsets follow the pair dialog: seconds the hi-res file starts after the voice file
//! (hi-res time = voice time − offset).

use std::path::Path;

use realfft::RealFftPlanner;
use serde_json::{Value, json};

use crate::error::{AppError, AppResult};

const HZ: usize = 100;
const WIN_S: usize = 60;
const STEP_S: usize = 30;
/// A window counts when its peak is this high and this far above the next best.
const MIN_PEAK: f32 = 0.35;
const MIN_RATIO: f32 = 2.0;

/// One stretch of the pairing, from `from_s` on the voice file's clock.
#[derive(Clone, Debug, PartialEq)]
pub struct Stretch {
    pub from_s: f64,
    pub offset_s: f64,
    /// 0–1: the share of the stretch's windows that agreed, scaled by their peaks.
    pub confidence: f64,
    /// Settled on the pictures (frame-exact) rather than the sound alone.
    pub picture: bool,
    /// Windows that agreed.
    pub windows: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Proposal {
    pub stretches: Vec<Stretch>,
    /// Voice-file spans the hi-res file has no match for (it lost them).
    pub unmatched: Vec<(f64, f64)>,
    pub fps: Option<f64>,
}

fn duration_s(t: &gmclip_media::demux::Track) -> f64 {
    let s = &t.samples;
    let (Some(&d), Some(&dur)) = (s.dts.last(), t.durations.last()) else {
        return 0.0;
    };
    (d + i64::from(dur)) as f64 / f64::from(t.timescale.max(1))
}

/// The 100 Hz envelope of a file's audio track (normalised; `cancel` stops it).
fn envelope(path: &Path, track: Option<u32>, dur_s: f64) -> AppResult<Vec<f32>> {
    const F: i64 = 705_600_000;
    const CHUNK: f64 = 240.0;
    // 4-minute chunks decoded on every core (decoding is the slow part)
    let chunks: Vec<(f64, f64)> = (0..)
        .map(|k| k as f64 * CHUNK)
        .take_while(|a| *a < dur_s)
        .map(|a| (a, (a + CHUNK).min(dur_s)))
        .collect();
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let parts: Vec<std::sync::Mutex<Option<AppResult<Vec<f32>>>>> =
        chunks.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..threads.min(chunks.len()) {
            sc.spawn(|| {
                loop {
                    let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(at, to)) = chunks.get(k) else {
                        break;
                    };
                    let r = crate::analysis::track_16k(
                        path,
                        track,
                        gmclip_model::Flicks((at * F as f64) as i64),
                        gmclip_model::Flicks((to * F as f64) as i64),
                    )
                    .map_err(|e| {
                        AppError::internal(format!(
                            "reading the sound of {}: {e:?}",
                            path.display()
                        ))
                    })
                    .map(|pcm| {
                        let want = ((to - at) * HZ as f64).round() as usize;
                        (0..want)
                            .map(|k| {
                                let a = k * 160;
                                let b = (a + 160).min(pcm.len());
                                let e = if a < b {
                                    (pcm[a..b].iter().map(|x| x * x).sum::<f32>() / (b - a) as f32)
                                        .sqrt()
                                } else {
                                    0.0
                                };
                                (e + 1e-4).ln()
                            })
                            .collect::<Vec<f32>>()
                    });
                    if let Ok(mut g) = parts[k].lock() {
                        *g = Some(r);
                    }
                }
            });
        }
    });
    let mut log = Vec::with_capacity((dur_s * HZ as f64) as usize + 1);
    for p in parts {
        let r = p
            .into_inner()
            .ok()
            .flatten()
            .ok_or_else(|| AppError::internal("a sound chunk was not read"))?;
        log.extend(r?);
    }
    // take off a 1 s moving mean, then normalise
    let n = log.len();
    let mut pre = vec![0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + f64::from(log[i]);
    }
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let a = i.saturating_sub(HZ / 2);
            let b = (i + HZ / 2).min(n);
            log[i] - ((pre[b] - pre[a]) / (b - a) as f64) as f32
        })
        .collect();
    let mean = out.iter().map(|&x| f64::from(x)).sum::<f64>() / n.max(1) as f64;
    let var = out
        .iter()
        .map(|&x| (f64::from(x) - mean).powi(2))
        .sum::<f64>()
        / n.max(1) as f64;
    let sd = var.sqrt().max(1e-9);
    for x in &mut out {
        *x = ((f64::from(*x) - mean) / sd) as f32;
    }
    Ok(out)
}

/// Cross-correlation of every window against one reference, sharing its FFT.
struct Correlator {
    n: usize,
    reference: Vec<realfft::num_complex::Complex<f32>>,
    len_ref: usize,
    planner: RealFftPlanner<f32>,
}

impl Correlator {
    fn new(reference: &[f32], win: usize) -> Self {
        let n = (reference.len() + win).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let mut buf = vec![0f32; n];
        buf[..reference.len()].copy_from_slice(reference);
        let mut spec = fwd.make_output_vec();
        let _ = fwd.process(&mut buf, &mut spec);
        Self {
            n,
            reference: spec,
            len_ref: reference.len(),
            planner,
        }
    }

    /// Correlation per reference position (`w` starting there), divided by `w.len()`.
    fn run(&mut self, w: &[f32]) -> Vec<f32> {
        let fwd = self.planner.plan_fft_forward(self.n);
        let inv = self.planner.plan_fft_inverse(self.n);
        let mut buf = vec![0f32; self.n];
        for (i, x) in w.iter().rev().enumerate() {
            buf[i] = *x;
        }
        let mut spec = fwd.make_output_vec();
        let _ = fwd.process(&mut buf, &mut spec);
        for (a, b) in spec.iter_mut().zip(&self.reference) {
            *a *= *b;
        }
        let mut out = inv.make_output_vec();
        let _ = inv.process(&mut spec, &mut out);
        let scale = (self.n * w.len()) as f32;
        let first = w.len() - 1;
        let last = (first + self.len_ref + 1)
            .saturating_sub(w.len())
            .min(out.len());
        out[first..last].iter().map(|x| x / scale).collect()
    }
}

/// The best lag of a correlation, its peak and the next best away from it (±0.5 s).
fn peak(c: &[f32]) -> Option<(usize, f32, f32)> {
    let (j, &p) = c.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let second = c
        .iter()
        .enumerate()
        .filter(|(i, _)| i.abs_diff(j) > HZ / 2)
        .map(|(_, &x)| x)
        .fold(f32::MIN, f32::max);
    Some((j, p, second))
}

/// Correlation of `a[s..s+n]` with `b` at a fixed offset (in envelope samples).
fn corr_at(a: &[f32], b: &[f32], s: usize, n: usize, off: i64) -> Option<f32> {
    let j = s as i64 - off;
    if j < 0 || j as usize + n > b.len() || s + n > a.len() {
        return None;
    }
    let j = j as usize;
    Some(
        a[s..s + n]
            .iter()
            .zip(&b[j..j + n])
            .map(|(x, y)| x * y)
            .sum::<f32>()
            / n as f32,
    )
}

/// Proposes the pairing of `voice` (its audio track `voice_track`) and `hires`.
pub fn propose(voice: &Path, voice_track: Option<u32>, hires: &Path) -> AppResult<Proposal> {
    let iv = crate::inputs::index_file(voice)?;
    let ih = crate::inputs::index_file(hires)?;
    let va = match voice_track {
        Some(id) => iv.audio_all.iter().find(|t| t.id == u64::from(id)),
        None => iv.audio.as_ref(),
    }
    .ok_or_else(|| AppError::invalid("the voice file has no sound to match"))?;
    let ha = ih.audio.as_ref().ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "the hi-res file has no sound of its own to match against; type the offset",
        )
    })?;
    let a = envelope(voice, voice_track, duration_s(va))?;
    let b = envelope(hires, None, duration_s(ha))?;
    let win = WIN_S * HZ;
    if a.len() < win || b.len() < win {
        return Err(AppError::invalid(
            "both files need at least a minute of sound",
        ));
    }
    let mut cor = Correlator::new(&b, win);
    // (start s, offset in samples, peak, ratio)
    let mut wins: Vec<(usize, i64, f32, f32)> = Vec::new();
    let mut s = 0;
    while s + win <= a.len() {
        let w = &a[s..s + win];
        if let Some((j, p, second)) = peak(&cor.run(w)) {
            wins.push((s, s as i64 - j as i64, p, p / second.max(1e-3)));
        }
        s += STEP_S * HZ;
    }
    let ok: Vec<&(usize, i64, f32, f32)> = wins
        .iter()
        .filter(|w| w.2 >= MIN_PEAK && w.3 >= MIN_RATIO)
        .collect();
    if ok.is_empty() {
        return Err(AppError::new(
            "Unsupported",
            "the two files' sound does not match anywhere; are they the same recording?",
        ));
    }
    // stretches of agreeing windows (a lone disagreeing window is dropped)
    let mut groups: Vec<Vec<&(usize, i64, f32, f32)>> = Vec::new();
    for w in ok {
        match groups.last_mut() {
            Some(g) if (g[g.len() - 1].1 - w.1).abs() <= 5 => g.push(w),
            _ => groups.push(vec![w]),
        }
    }
    if groups.len() > 1 {
        groups.retain(|g| g.len() >= 2);
    }
    // merge neighbours that ended up with the same offset after dropping lone windows
    let mut merged: Vec<Vec<&(usize, i64, f32, f32)>> = Vec::new();
    for g in groups {
        match merged.last_mut() {
            Some(m) if (m[m.len() - 1].1 - g[0].1).abs() <= 5 => m.extend(g),
            _ => merged.push(g),
        }
    }
    let mut stretches = Vec::new();
    let mut unmatched = Vec::new();
    let mut from_s = 0.0;
    for (k, g) in merged.iter().enumerate() {
        // the median offset of the stretch
        let mut offs: Vec<i64> = g.iter().map(|w| w.1).collect();
        offs.sort_unstable();
        let off = offs[offs.len() / 2];
        if k > 0 {
            // place the change: 5 s windows from the last window of the previous stretch
            // to the first of this one, scored at both offsets
            let prev = &merged[k - 1];
            let prev_off = stretches.last().map_or(prev[0].1, |s: &Stretch| {
                (s.offset_s * HZ as f64).round() as i64
            });
            let a0 = prev[prev.len() - 1].0;
            let a1 = g[0].0 + win;
            let n = 5 * HZ;
            let mut last_prev = a0;
            let mut first_this = a1;
            let mut t = a0;
            while t + n <= a1.min(a.len()) {
                let p = corr_at(&a, &b, t, n, prev_off).unwrap_or(0.0);
                let q = corr_at(&a, &b, t, n, off).unwrap_or(0.0);
                if p >= MIN_PEAK && p > q {
                    last_prev = t + n;
                }
                if q >= MIN_PEAK && q > p && first_this == a1 {
                    first_this = t;
                }
                t += n / 2;
            }
            let change = if first_this > last_prev {
                if first_this - last_prev >= 2 * HZ {
                    unmatched.push((last_prev as f64 / HZ as f64, first_this as f64 / HZ as f64));
                }
                first_this
            } else {
                (last_prev + first_this) / 2
            };
            from_s = change as f64 / HZ as f64;
        }
        let mean_peak = g.iter().map(|w| w.2).sum::<f32>() / g.len() as f32;
        // windows starting inside this stretch's own agreeing span
        let (g0, g1) = (g[0].0, g[g.len() - 1].0);
        let span = wins
            .iter()
            .filter(|w| w.0 >= g0 && w.0 <= g1)
            .count()
            .max(1);
        stretches.push(Stretch {
            from_s,
            offset_s: off as f64 / HZ as f64,
            confidence: (f64::from(mean_peak.min(1.0)) * g.len() as f64 / span as f64)
                .clamp(0.0, 1.0),
            picture: false,
            windows: g.len(),
        });
    }
    if let Some(s) = stretches.first_mut() {
        s.from_s = 0.0;
    }
    let fps = picture_fps(&iv, &ih);
    if let Some(fps) = fps {
        for (k, st) in stretches.iter_mut().enumerate() {
            let end = merged[k][merged[k].len() - 1].0 as f64 / HZ as f64 + WIN_S as f64;
            // two seconds in the middle of the stretch's agreeing windows
            let mid = (merged[k][merged[k].len() / 2].0 as f64 / HZ as f64 + WIN_S as f64 / 2.0)
                .min(end - 3.0);
            if let Some(f) = settle(voice, hires, fps, mid, st.offset_s) {
                st.offset_s = f;
                st.picture = true;
            }
        }
    }
    Ok(Proposal {
        stretches,
        unmatched,
        fps,
    })
}

fn picture_fps(iv: &crate::inputs::Indexed, ih: &crate::inputs::Indexed) -> Option<f64> {
    let fps = |t: &gmclip_media::demux::Track| {
        crate::video::track_fps(t).map(|r| r.num as f64 / r.den as f64)
    };
    let (a, b) = (iv.video.as_ref()?, ih.video.as_ref()?);
    let (fa, fb) = (fps(a)?, fps(b)?);
    ((fa - fb).abs() < 0.01).then_some(fa)
}

/// Small grey frames `[first, first + n)` of a file's picture.
#[cfg(any(target_os = "macos", windows))]
fn greys(path: &Path, fps: f64, first: i64, n: u64) -> Option<Vec<Vec<u8>>> {
    let ix = crate::inputs::index_file(path).ok()?;
    let v = ix.video?;
    let a = ix.audio?;
    let (w, h) = (160u32, 90u32);
    let fps_i = fps.round() as i64;
    let _ = a;
    let first_sample = first.max(0) * 48_000 / fps_i.max(1);
    let plan = json!({
        "schema": "gmclip.renderplan/0",
        "meta": {"title": "pair check"},
        "output": {"canvas": {"w": w, "h": h}, "fps": {"num": fps_i, "den": 1}, "frames": n,
                   "video": {"codec": "h264"}, "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "s", "path": path.to_string_lossy()}],
        "parts": [{"id": "p", "out": [0, n],
                   "picture": {"source": "s", "first_frame": first.max(0)},
                   "audio": {"source": "s", "first_sample": first_sample, "rate": 48000},
                   "layers": [{"id": "l", "role": "other", "crop": {"x": 0, "y": 0, "w": v.width, "h": v.height},
                               "place": {"x": 0, "y": 0, "w": w, "h": h}, "z": 0}]}],
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    let plan = gmclip_plan::parse(&serde_json::to_vec(&plan).ok()?).ok()?;
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let at: Vec<u64> = (0..n).collect();
    let frames = gmclip_render::export::render_frames(&plan, &mut inputs, &at).ok()?;
    Some(
        frames
            .into_iter()
            .map(|(_, im)| {
                im.px
                    .chunks(3)
                    .map(|p| ((u16::from(p[0]) + u16::from(p[1]) * 2 + u16::from(p[2])) / 4) as u8)
                    .collect()
            })
            .collect(),
    )
}

#[cfg(not(any(target_os = "macos", windows)))]
fn greys(_: &Path, _: f64, _: i64, _: u64) -> Option<Vec<Vec<u8>>> {
    None
}

/// The frame-exact offset near `offset_s`, from the pictures around voice time `at_s`.
fn settle(voice: &Path, hires: &Path, fps: f64, at_s: f64, offset_s: f64) -> Option<f64> {
    const N: i64 = 60;
    const R: i64 = 8;
    let vf = (at_s * fps).round() as i64;
    let base = (offset_s * fps).round() as i64;
    let hf = vf - base - R;
    if hf < 0 {
        return None;
    }
    let p = greys(voice, fps, vf, N as u64)?;
    let m = greys(hires, fps, hf, (N + 2 * R) as u64)?;
    let err = |d: i64| -> f64 {
        let mut s = 0u64;
        for i in 0..N as usize {
            let (x, y) = (&p[i], &m[i + (R + d) as usize]);
            s += x
                .iter()
                .zip(y)
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum::<u64>();
        }
        s as f64
    };
    let mut errs: Vec<(i64, f64)> = (-R..=R).map(|d| (d, err(d))).collect();
    errs.sort_by(|a, b| a.1.total_cmp(&b.1));
    let (d, best) = errs[0];
    let median = errs[errs.len() / 2].1;
    // the picture moved enough to tell, and the best stands out
    (median > 0.0 && best < median * 0.6).then(|| (base - d) as f64 / fps)
}

impl Proposal {
    pub fn to_json(&self) -> Value {
        json!({
            "offset_s": self.stretches.first().map(|s| s.offset_s),
            "stretches": self.stretches.iter().map(|s| json!({
                "from_s": s.from_s, "offset_s": s.offset_s, "confidence": s.confidence,
                "picture": s.picture, "windows": s.windows,
                "frames": self.fps.map(|f| (s.offset_s * f).round()),
            })).collect::<Vec<_>>(),
            "unmatched": self.unmatched.iter().map(|(a, b)| json!({"from_s": a, "to_s": b})).collect::<Vec<_>>(),
            "fps": self.fps,
        })
    }
}

/// The IPC form: `{voice, hires, voice_track?}` → the proposal.
pub fn propose_json(voice: &str, hires: &str, voice_track: Option<u32>) -> AppResult<Value> {
    let t = std::time::Instant::now();
    let p = propose(Path::new(voice), voice_track, Path::new(hires))?;
    let mut v = p.to_json();
    v["took_s"] = json!(t.elapsed().as_secs_f64());
    Ok(v)
}
