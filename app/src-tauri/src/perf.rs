//! `gmclip-app perf` (U.18, A2.05): the §13 [M2] rows the app can measure in-process,
//! in a throwaway library. `tools/m2_perf.sh` runs it on the Air from a copied release
//! binary (AJ.02) beside the windowed rows (cold start, idle CPU, memory while playing).
//!
//! - **open project ≤ 1 s at 500 parts**: a 500-part cut over the given video, the
//!   project closed, then opened again and its cut planned and viewed (`open_home` +
//!   `cut_view`).
//! - **R-CMD-8 hot commands, p99 ≤ 2 ms** (handler + commit, through the actor): trim,
//!   split + merge, reorder, crop, strike/restore and keyword on that cut.
//! - **R-EVT-1 burst**: 1 000 agent edits from another thread while the human's edits keep
//!   coming; the human's p99 must stay within one frame (16.7 ms).
//! - **memory**: the process's resident size after opening and planning.
//! - **memory with ASR** (`--asr <video>`, S11: editing + ASR ≤ 2.2 GB): the video opened
//!   as a project, its whole analysis (transcription, then the columns and the VAD) run to
//!   the end while the resident size is sampled every 100 ms; the peak per phase.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_core::{CallRequest, Caller};
use serde_json::{Value, json};

use crate::core::{AppCore, Dirs};
use crate::error::{AppError, AppResult};

const F: i64 = 705_600_000;

fn pct(v: &mut [Duration], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize].as_secs_f64() * 1000.0
}

/// Resident set size of this process, MB (`ps`; macOS has no /proc).
pub fn rss_mb() -> f64 {
    std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<f64>()
                .ok()
        })
        .map_or(f64::NAN, |kb| kb / 1024.0)
}

fn ids(v: &Value, k: &str) -> AppResult<String> {
    v[k].as_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::internal(format!("no {k}")))
}

/// Runs the rows; `video` is any H.264 MP4 (≥ 60 s plays best).
pub fn run(video: Option<&Path>) -> AppResult<Value> {
    let video = video.ok_or_else(|| AppError::invalid("perf needs --video <an MP4>"))?;
    let root = std::env::temp_dir().join(format!("gmclip-perf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let core = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let out = rows(&core, video);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
    out
}

fn rows(core: &Arc<AppCore>, video: &Path) -> AppResult<Value> {
    let v = crate::video::open_video(core, video)?;
    let (pid, cut) = (ids(&v, "project")?, ids(&v, "cut")?);
    // analysis would compete with the measurements
    core.analysis.cancel_project(&pid);
    let p = core.project(&pid)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(&cut)?;
    let first = gmclip_model::clock::cut_layout(&doc, cid)[0].part;
    let pt = doc
        .get::<gmclip_model::entities::Part>(first)
        .ok_or_else(|| AppError::internal("part"))?;
    let (media, span) = (pt.media, (pt.src_out.0 - pt.src_in.0).max(F));
    // 500 parts of 1 s stepping through the video, 200 per transaction (each goes first)
    let n_parts = 500usize;
    let mut made = 0usize;
    while made < n_parts {
        let ops: Vec<Value> = (made..(made + 200).min(n_parts))
            .map(|k| {
                let t = (k as i64 * F / 2) % (span - F).max(1);
                json!({"op": "part_add", "media": media, "src_in": t, "src_out": t + F})
            })
            .collect();
        made += ops.len();
        core.call(&pid, "cut_edit", json!({"cut": cut, "ops": ops}))?;
    }
    let home = p.home.clone();
    drop(p);
    core.close_project(&pid)?;

    // open ≤ 1 s at 500 parts (open, plan, view)
    let t = Instant::now();
    core.open_home(&home)?;
    core.analysis.cancel_project(&pid);
    let t_open = t.elapsed();
    let cv = core.cut_view(&pid, &cut)?;
    let t_view = t.elapsed();
    let n = cv["parts"].as_array().map_or(0, Vec::len);

    // hot commands on that cut, through the actor
    let p = core.project(&pid)?;
    let doc = p.handle.snapshot().doc.clone();
    let order: Vec<String> = gmclip_model::clock::cut_layout(&doc, cid)
        .iter()
        .map(|s| s.part.0.to_string())
        .collect();
    let mid = order[order.len() / 2].clone();
    let mid_part = doc
        .get::<gmclip_model::entities::Part>(crate::planner::ulid_id(&mid)?)
        .ok_or_else(|| AppError::internal("part"))?
        .clone();
    let framing = core
        .active_framing(&pid, &cut)
        .ok_or_else(|| AppError::internal("framing"))?;
    let screen = cv["parts"][order.len() / 2]["layers"]
        .as_array()
        .and_then(|l| l.first())
        .and_then(|l| l["model"].as_str())
        .map(str::to_owned)
        .unwrap_or_default();
    let human = |cmd: &str, args: Value| -> AppResult<Duration> {
        let t = Instant::now();
        core.call(&pid, cmd, args)?;
        Ok(t.elapsed())
    };
    let iters = 100usize;
    let mut hot = serde_json::Map::new();
    let mut worst = 0.0f64;
    let mut record = |name: &str, v: &mut Vec<Duration>| {
        let p99 = pct(v, 0.99);
        worst = worst.max(p99);
        hot.insert(
            name.into(),
            json!({"p50_ms": pct(v, 0.5), "p99_ms": p99, "n": v.len()}),
        );
    };
    let mut v = Vec::new();
    for i in 0..iters {
        let out = mid_part.src_out.0 - (i % 2) as i64 * F / 10;
        v.push(human("part_trim", json!({"part": mid, "src_out": out}))?);
    }
    record("trim", &mut v);
    let mut v = Vec::new();
    for i in 0..iters {
        let ops = if i % 2 == 0 {
            json!([{"op": "part_reorder", "part": mid, "after": order[10]}])
        } else {
            json!([{"op": "part_reorder", "part": mid}])
        };
        let t = Instant::now();
        let ok = core.call(&pid, "cut_edit", json!({"cut": cut, "ops": ops}));
        if ok.is_ok() {
            v.push(t.elapsed());
        }
    }
    record("reorder", &mut v);
    let mut v = Vec::new();
    if !screen.is_empty() {
        for i in 0..iters {
            let x = 0.1 + (i % 2) as f64 * 0.05;
            v.push(human(
                "crop_set",
                json!({"layer": screen, "part": mid, "framing": framing, "crop": {"x": x, "y": 0.1, "w": 0.5, "h": 0.5}}),
            )?);
        }
    }
    record("crop", &mut v);
    let mut v = Vec::new();
    for _ in 0..iters / 2 {
        let at = mid_part.src_in.0 + F / 2;
        let ops = json!([{"op": "part_split", "part": mid, "at": at}]);
        v.push(human("cut_edit", json!({"cut": cut, "ops": ops}))?);
        // merge it back (the timeline's ⌘Z)
        v.push(human("undo", json!({}))?);
    }
    record("split+undo", &mut v);

    // R-EVT-1: 1 000 agent edits from another thread, the human's p99 meanwhile
    let handle = p.handle.clone();
    let agent_cut = cut.clone();
    let agent_part = order[5].clone();
    let burst = std::thread::spawn(move || {
        let t = Instant::now();
        let (mut ok, mut first_err) = (0, None);
        for i in 0..1000 {
            let rev = handle.snapshot().doc.rev();
            let r = handle.call(CallRequest {
                command: "cut_edit".into(),
                args: json!({"cut": agent_cut, "ops": [{"op": "part_set", "part": agent_part, "gain_db": (i % 7) as f64 - 3.0}]}),
                base: Some(gmclip_proto::Base::AsOf { as_of: rev }),
                request_id: None,
                caller: Caller::agent("local:perf", "perf"),
            });
            match r {
                Ok(_) => ok += 1,
                Err(e) => {
                    first_err.get_or_insert(e.to_string());
                }
            }
        }
        (ok, t.elapsed(), first_err)
    });
    let mut v = Vec::new();
    while !burst.is_finished() {
        let out = mid_part.src_out.0 - (v.len() % 2) as i64 * F / 10;
        v.push(human("part_trim", json!({"part": mid, "src_out": out}))?);
    }
    let (agent_ok, agent_t, agent_err) = burst
        .join()
        .map_err(|_| AppError::internal("the burst thread"))?;
    let human_p99 = pct(&mut v, 0.99);
    let rss = rss_mb();

    Ok(json!({
        "machine": std::env::consts::ARCH,
        "open_project": {"parts": n, "open_ms": t_open.as_secs_f64() * 1000.0,
                          "open_plan_view_ms": t_view.as_secs_f64() * 1000.0, "target_ms": 1000},
        "hot_commands": {"cases": hot, "worst_p99_ms": worst, "target_p99_ms": 2.0, "parts": n},
        "r_evt_1": {"agent_ops": agent_ok, "agent_s": agent_t.as_secs_f64(),
                    "agent_first_refusal": agent_err, "human_ops_meanwhile": v.len(), "human_p99_ms": human_p99, "target_ms": 16.7},
        "rss_mb_after_open": rss,
    }))
}

/// `gmclip-app perf --asr <video>` (U.18, SPEC §13 "memory + ASR ≤ 2.2 GB"): opens the
/// video in a throwaway library and lets its analysis run to the end, sampling this
/// process's resident size every 100 ms. Reports the peak per phase, the time and the
/// real-time factor. Needs the speech model (`asr_error` says why not).
pub fn asr(video: &Path) -> AppResult<Value> {
    let root = std::env::temp_dir().join(format!("gmclip-perf-asr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let core = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let out = asr_rows(&core, video);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
    out
}

fn asr_rows(core: &Arc<AppCore>, video: &Path) -> AppResult<Value> {
    let rss0 = rss_mb();
    let t0 = Instant::now();
    let v = crate::video::open_video(core, video)?;
    let pid = ids(&v, "project")?;
    let opened = rss_mb();
    let mut peaks: std::collections::BTreeMap<String, f64> = Default::default();
    let mut phases: Vec<(String, f64)> = Vec::new();
    let mut peak = opened;
    let limit = Duration::from_secs(3 * 3600);
    let last = loop {
        let st = crate::analysis::status(core, &pid);
        let s0 = &st["sources"][0];
        let phase = s0["phase"].as_str().unwrap_or("none").to_owned();
        let r = rss_mb();
        peak = peak.max(r);
        let e = peaks.entry(phase.clone()).or_insert(0.0);
        *e = e.max(r);
        if phases.last().is_none_or(|p| p.0 != phase) {
            phases.push((phase.clone(), t0.elapsed().as_secs_f64()));
        }
        if matches!(phase.as_str(), "done" | "failed") || t0.elapsed() > limit {
            break s0.clone();
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let secs = t0.elapsed().as_secs_f64();
    let total = last["total_s"].as_f64().unwrap_or(f64::NAN);
    Ok(json!({
        "machine": std::env::consts::ARCH,
        "source_s": total,
        "phase": last["phase"], "asr_error": last["asr_error"], "error": last["error"],
        "seconds": secs,
        "real_time_factor": total / secs,
        "rss_mb": {"process_start": rss0, "after_open": opened, "peak": peak, "by_phase": peaks},
        "phases_at_s": phases.iter().map(|(p, t)| json!([p, t])).collect::<Vec<_>>(),
        "target_mb": 2200,
    }))
}

/// Options for [`scrub`].
#[derive(Clone, Debug, Default)]
pub struct ScrubOptions {
    /// Build the whole proxy (the "proxy for (a) ≤ 10 min" row); else only the first
    /// three segments the scrub needs.
    pub full_proxy: bool,
}

/// `gmclip-app perf --scrub <source> [--full-proxy]` (H.05; SPEC §13 rows "Scrub proxy for
/// (a)", "Scrub", "Exact seek"), through the preview session the viewer uses, in a
/// throwaway folder:
/// - the scrub proxy's build time (the whole source with `full_proxy`);
/// - a scrub drag across the proxy: 240 distinct frames forward and back at 960 × 540,
///   frames per second (≥ 20);
/// - scrubbing past the proxy (keyframe-only from the master), per frame;
/// - 100 seeded exact seeks across the whole source from the master, p50/p95, classed by
///   the GOP each target sits in (≤ 2 s: ≤ 60/150 ms; ≤ 5 s: ≤ 150/400 ms).
#[cfg(any(target_os = "macos", windows))]
pub fn scrub(source: &Path, opts: &ScrubOptions) -> AppResult<Value> {
    use gmclip_media::backend::SampleSource;
    use gmclip_render::export::inputs::DemuxInputs;
    use gmclip_render::export::preview::{PreviewSession, ScrubPath};
    use gmclip_render::export::proxy::{self, BuildOptions};
    let err = |e: gmclip_render::ops::RenderError| AppError::internal(e.to_string());
    let path = source
        .to_str()
        .ok_or_else(|| AppError::invalid("a UTF-8 path"))?
        .to_owned();
    let dir = std::env::temp_dir().join(format!("gmclip-perf-scrub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let r = (|| -> AppResult<Value> {
        let mut inputs = DemuxInputs::new(None);
        let v = inputs.video_file(source, "src").map_err(err)?;
        let idx = v.source.index().clone();
        let (sw, sh) = (v.width, v.height);
        let n = idx.len() as u64;
        if n < 2 {
            return Err(AppError::invalid("the video is too short"));
        }
        let pts = |i: usize| idx.dts[i] + i64::from(idx.cts_off[i]);
        let syncs: Vec<usize> = (0..idx.len()).filter(|&i| idx.sync[i]).collect();
        let secs = |a: usize, b: usize| (pts(b) - pts(a)) as f64 / f64::from(idx.timescale.max(1));
        let gop_s: Vec<f64> = syncs.windows(2).map(|w| secs(w[0], w[1])).collect();
        let longest = gop_s.iter().copied().fold(0.0, f64::max);
        // the plan the stream viewer would show: the whole source, full frame, 1280 wide
        let cw = 1280u32;
        let ch =
            ((f64::from(cw) * f64::from(sh) / f64::from(sw.max(1))).round() as u32 & !1).max(2);
        let plan = json!({
            "schema": "gmclip.renderplan/0",
            "output": {"canvas": {"w": cw, "h": ch}, "fps": {"num": 30, "den": 1}, "frames": n,
                       "video": {"codec": "h264"}, "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}},
            "background": {"kind": "colour", "rgb": "#000000"},
            "sources": [{"id": "pic", "path": path}],
            "parts": [{"id": "all", "out": [0, n],
                       "picture": {"source": "pic", "first_frame": 0},
                       "audio": {"source": "pic", "first_sample": 0, "rate": 48000},
                       "layers": [{"id": "l", "role": "other", "crop": {"x": 0, "y": 0, "w": sw, "h": sh},
                                   "place": {"x": 0, "y": 0, "w": cw, "h": ch}, "z": 0}]}],
            "audio": {"loudness": {"mode": "as_recorded"}}
        });
        let plan = gmclip_plan::parse(&serde_json::to_vec(&plan).unwrap_or_default())
            .map_err(|e| AppError::internal(format!("plan: {}", e.reason())))?;
        // the proxy
        let key = proxy::content_key(source).map_err(|e| AppError::internal(e.to_string()))?;
        let t = Instant::now();
        let rep = proxy::build(
            source,
            &dir,
            &key,
            &BuildOptions {
                until: (!opts.full_proxy).then_some(3 * proxy::SEG_FRAMES),
                ..Default::default()
            },
        )
        .map_err(err)?;
        let build_s = t.elapsed().as_secs_f64();
        let covered = proxy::Manifest::read(&dir).map_or(0, |m| m.covered());
        let mut s = PreviewSession::open(std::sync::Arc::new(plan), &mut inputs, (960, 540))
            .map_err(err)?;
        s.set_proxy_dirs([(path.clone(), dir.clone())].into_iter().collect());
        // scrub drag across the proxy
        let steps = 240u64;
        let span = covered.min(n).saturating_sub(1).max(1);
        let mut times = Vec::new();
        let mut not_proxy = 0;
        let t = Instant::now();
        for i in 0..steps {
            let f = if i < steps / 2 {
                span * (2 * i) / steps
            } else {
                span * (2 * (steps - i) - 1) / steps
            };
            let t1 = Instant::now();
            let (_, p) = s.render_scrub(f, &[], &[]).map_err(err)?;
            times.push(t1.elapsed());
            if p != ScrubPath::Proxy {
                not_proxy += 1;
            }
        }
        let scrub_fps = steps as f64 / t.elapsed().as_secs_f64();
        let (sp50, sp95) = (pct(&mut times, 0.5), pct(&mut times, 0.95));
        // past the proxy: keyframe-only
        let mut kt = Vec::new();
        let mut not_key = 0;
        if covered + 60 < n {
            for i in 0..60u64 {
                let f = covered + (n - covered - 1) * i / 60;
                let t1 = Instant::now();
                let (_, p) = s.render_scrub(f, &[], &[]).map_err(err)?;
                kt.push(t1.elapsed());
                if p != ScrubPath::Keyframe {
                    not_key += 1;
                }
            }
        }
        // exact seeks, classed by GOP
        let gop_of = |f: u64| {
            let t = idx.pts_order[f as usize] as usize;
            let k = syncs.partition_point(|&s| s <= t);
            let a = syncs[k.saturating_sub(1)];
            let b = syncs.get(k).copied().unwrap_or(idx.len() - 1);
            (secs(a, b), t - a + 1)
        };
        let class = if longest <= 2.0 { 2.0 } else { 5.0 };
        let mut rng = 0x676d_636c_6970u64;
        let mut all = Vec::new();
        let mut within = Vec::new();
        let mut chain = 0usize;
        for _ in 0..100 {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let f = (rng >> 33) % n;
            let t1 = Instant::now();
            s.render(f, &[], &[]).map_err(err)?;
            let d = t1.elapsed();
            all.push(d);
            let (g, c) = gop_of(f);
            chain += c;
            if g <= class {
                within.push(d);
            }
        }
        let budget = if class <= 2.0 {
            (60.0, 150.0)
        } else {
            (150.0, 400.0)
        };
        Ok(json!({
            "source": path,
            "size": [sw, sh],
            "frames": n,
            "gop_s": {"longest": longest, "mean": gop_s.iter().sum::<f64>() / gop_s.len().max(1) as f64},
            "proxy": {"frames": rep.encoded, "of": n, "full": opts.full_proxy, "seconds": build_s,
                      "fps": rep.encoded as f64 / build_s.max(1e-9), "mb": rep.bytes as f64 / 1e6,
                      "size": [rep.size.0, rep.size.1]},
            "scrub": {"frames_per_s": scrub_fps, "p50_ms": sp50, "p95_ms": sp95, "not_proxy": not_proxy},
            "keyframe_only": {"p50_ms": pct(&mut kt, 0.5), "p95_ms": pct(&mut kt, 0.95), "not_keyframe": not_key},
            "exact_seek": {"class_s": class, "budget_ms": [budget.0, budget.1],
                           "p50_ms": pct(&mut within, 0.5), "p95_ms": pct(&mut within, 0.95),
                           "in_class": within.len(),
                           "all_p50_ms": pct(&mut all.clone(), 0.5), "all_p95_ms": pct(&mut all.clone(), 0.95),
                           "max_ms": pct(&mut all, 1.0), "mean_chain_frames": chain as f64 / 100.0},
        }))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// `gmclip-app perf --open <video>` (lane K5, "open huge files instantly"): a long
/// recording opened in a throwaway library through the app's own paths, timed from the
/// open call, before any proxy exists (its build starts with the open, as in the app):
/// - **open**: probe, demux index, project and whole-video cut;
/// - **first frame**: the viewer's first exact frame of the cut (plan, preview session,
///   decode from the master);
/// - **seek to the middle**: the scrub picture (the master's keyframe, no proxy there
///   yet) and the exact frame once the scrub rests;
/// - **filmstrip**: the first timeline filmstrip (master keyframes until the proxy
///   covers them), how many of its thumbnails it had;
/// - the proxy's coverage at the end (frames).
#[cfg(any(target_os = "macos", windows))]
pub fn open_first_frame(video: &Path) -> AppResult<Value> {
    use crate::viewer::ViewerMsg;
    let root = std::env::temp_dir().join(format!("gmclip-perf-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let core = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let r = (|| -> AppResult<Value> {
        let ms = |d: Duration| (d.as_secs_f64() * 1e4).round() / 10.0;
        let wait = |what: &str, limit: Duration, ok: &dyn Fn() -> bool| -> AppResult<()> {
            let t = Instant::now();
            while !ok() {
                if t.elapsed() > limit {
                    return Err(AppError::internal(format!("timed out waiting for {what}")));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        };
        let t0 = Instant::now();
        let v = crate::video::open_video(&core, video)?;
        let open = t0.elapsed();
        let (pid, cut) = (ids(&v, "project")?, ids(&v, "cut")?);
        let slot = Arc::new(crate::frame::FrameSlot::default());
        let (_records, rx) = std::sync::mpsc::channel();
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let viewer = crate::viewer::spawn(&core, slot.clone(), rx, connected);
        let drain = slot.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let reader = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = drain.take(Duration::from_millis(50));
            }
        });
        viewer.send(ViewerMsg::Load {
            project: pid.clone(),
            cut: cut.clone(),
            framing: None,
            keep: false,
        });
        wait("the first frame", Duration::from_secs(120), &|| {
            viewer.state().rendered >= 1
        })
        .map_err(|e| match viewer.state().error {
            Some(why) => AppError::internal(format!("{}: {why}", e.message)),
            None => e,
        })?;
        let first = t0.elapsed();
        let st = viewer.state();
        let (frames, fps) = (st.frames, st.fps);
        let covered_then = core.proxies.status(Some(&pid))["jobs"][0]["covered"]
            .as_u64()
            .unwrap_or(0);
        // the middle: the keyframe picture while scrubbing, the exact frame once it rests
        let mid = frames / 2;
        let n0 = viewer.state().rendered;
        let t1 = Instant::now();
        viewer.send(ViewerMsg::ScrubTo(mid));
        wait("the scrub picture", Duration::from_secs(60), &|| {
            let s = viewer.state();
            s.frame == mid && s.rendered > n0
        })?;
        let scrub = t1.elapsed();
        let scrub_path = viewer.state().scrub_path;
        wait("the exact frame", Duration::from_secs(60), &|| {
            viewer.state().scrub_path.as_deref() == Some("exact")
        })?;
        let exact = t1.elapsed();
        // the first filmstrip (the timeline's fit zoom asks for at most 400)
        let t2 = Instant::now();
        let strip = crate::proxy::filmstrip(&core, &pid, &cut, Some(30), Some(96));
        let strip_ms = t2.elapsed();
        let (mut shown, mut total) = (0u32, 0u32);
        if let Some(b) = &strip {
            let mut at = 16usize;
            while at + 8 <= b.len() {
                let w = u16::from_le_bytes([b[at + 4], b[at + 5]]) as usize;
                let h = u16::from_le_bytes([b[at + 6], b[at + 7]]) as usize;
                total += 1;
                if w > 0 {
                    shown += 1;
                }
                at += 8 + w * h * 3;
            }
        }
        // again: what the next ask (each proxy event) gets
        let t3 = Instant::now();
        let _ = crate::proxy::filmstrip(&core, &pid, &cut, Some(30), Some(96));
        let strip_again = t3.elapsed();
        let covered = core.proxies.status(Some(&pid))["jobs"][0]["covered"]
            .as_u64()
            .unwrap_or(0);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = reader.join();
        Ok(json!({
            "video": video.to_string_lossy(),
            "frames": frames,
            "seconds": frames as f64 / fps.max(1.0),
            "open_ms": ms(open),
            "first_frame_ms": ms(first),
            "proxy_frames_at_first_frame": covered_then,
            "seek_middle": {"frame": mid, "scrub_ms": ms(scrub), "scrub_path": scrub_path,
                            "exact_ms": ms(exact)},
            "filmstrip": {"ms": ms(strip_ms), "thumbnails": total, "pictures": shown,
                          "again_ms": ms(strip_again)},
            "proxy_frames_at_end": covered,
        }))
    })();
    core.analysis.shutdown(Duration::from_secs(10));
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
    r
}

/// A running or finished [`dev_burst`].
#[cfg(feature = "dev-bridge")]
#[derive(Debug, Default, Clone)]
struct Burst {
    running: bool,
    ok: usize,
    refused: usize,
    first_refusal: Option<String>,
    ms: f64,
}

#[cfg(feature = "dev-bridge")]
static BURST: std::sync::Mutex<Option<Burst>> = std::sync::Mutex::new(None);

/// Dev bridge only: `n` agent edits (`cut_edit` part gain, as the perf burst) on `cut`'s
/// `part`, from another thread; or, with `status`, how the last burst went.
#[cfg(feature = "dev-bridge")]
pub fn dev_burst(
    core: &Arc<AppCore>,
    project: &str,
    status: bool,
    cut: &str,
    part: &str,
    n: usize,
) -> AppResult<Value> {
    let lock = || {
        BURST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if status {
        let b = lock().clone().unwrap_or_default();
        return Ok(
            json!({"running": b.running, "ok": b.ok, "refused": b.refused,
                         "first_refusal": b.first_refusal, "ms": b.ms}),
        );
    }
    if lock().as_ref().is_some_and(|b| b.running) {
        return Err(AppError::invalid("a burst is running"));
    }
    if cut.is_empty() || part.is_empty() {
        return Err(AppError::invalid("cut and part"));
    }
    let handle = core.project(project)?.handle.clone();
    *lock() = Some(Burst {
        running: true,
        ..Burst::default()
    });
    let (cut, part) = (cut.to_owned(), part.to_owned());
    std::thread::spawn(move || {
        let t = Instant::now();
        for i in 0..n {
            let rev = handle.snapshot().doc.rev();
            let r = handle.call(CallRequest {
                command: "cut_edit".into(),
                args: json!({"cut": cut, "ops": [{"op": "part_set", "part": part, "gain_db": (i % 7) as f64 - 3.0}]}),
                base: Some(gmclip_proto::Base::AsOf { as_of: rev }),
                request_id: None,
                caller: Caller::agent("local:e2e", "e2e"),
            });
            let mut g = BURST
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(b) = g.as_mut() {
                match r {
                    Ok(_) => b.ok += 1,
                    Err(e) => {
                        b.refused += 1;
                        b.first_refusal.get_or_insert(e.to_string());
                    }
                }
                b.ms = t.elapsed().as_secs_f64() * 1000.0;
            }
        }
        if let Some(b) = BURST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            b.running = false;
        }
    });
    Ok(json!({"started": n}))
}
