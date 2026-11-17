//! The M2 vertical slice through the app core, without a window: open a project, import
//! an RT cut (the converter's h79 v4 RenderPlan), plan it, trim a part, split, move a
//! layer, undo; then (lab) preview frames and export.
//!
//! Needs AJ's media (the h79 sources on this Mac); skips with a note when absent.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs, sig_of};
use serde_json::Value;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn h79() -> Option<PathBuf> {
    let p = repo().join("schemas/examples/h79-v4.renderplan.json");
    let dir = std::env::temp_dir().join(format!("gmclip-slice-plan-{}", std::process::id()));
    let p = gmclip_app::labfixture::plan_copy(&p, &dir).ok()?;
    let plan = gmclip_plan::parse(&std::fs::read(&p).ok()?).ok()?;
    plan.sources
        .iter()
        .all(|s| Path::new(&s.path).exists())
        .then_some(p)
}

fn core(name: &str) -> Arc<AppCore> {
    let root = std::env::temp_dir().join(format!("gmclip-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    })
}

fn setup(name: &str) -> Option<(Arc<AppCore>, String, String)> {
    let Some(plan) = h79() else {
        eprintln!("skipped: h79 v4 sources are not on this machine");
        return None;
    };
    let c = core(name);
    let p = c.create_project("Slice test").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let r = c.import(&pid, &plan).unwrap();
    let cut = r["cut"].as_str().unwrap().to_owned();
    Some((c, pid, cut))
}

#[test]
fn import_plans_identically_then_trim_split_place_undo() {
    let Some((c, pid, cut)) = setup("slice") else {
        return;
    };
    let imp = c.imported(&pid, &cut).unwrap();
    let base_sig = sig_of(&imp.base);
    // An untouched import plans to the identical RenderPlan.
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(!pl.edited);
    assert_eq!(*pl.plan, imp.base);
    assert_eq!(sig_of(&pl.plan), base_sig);
    let v = c.cut_view(&pid, &cut).unwrap();
    let parts = v["parts"].as_array().unwrap().clone();
    assert_eq!(parts.len(), imp.base.parts.len());
    assert_eq!(v["frames"], 1807);

    // Trim part 2's out edge by 10 frames.
    let p2 = &parts[1];
    c.trim_by(&pid, &cut, p2["id"].as_str().unwrap(), "out", -10)
        .unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(pl.edited);
    assert_eq!(pl.plan.output.frames, 1797);
    assert!(gmclip_plan::check_plan(&pl.plan).is_empty());
    assert_ne!(sig_of(&pl.plan), base_sig);
    let evs = pl.plan.text_events.as_ref().unwrap();
    assert!(evs.iter().all(|e| e.out.end() <= 1797));
    // Parts after the trim moved 10 frames earlier; their pictures did not change.
    assert_eq!(
        pl.plan.parts[2].out.start(),
        imp.base.parts[2].out.start() - 10
    );
    assert_eq!(
        pl.plan.parts[2].picture.first_frame,
        imp.base.parts[2].picture.first_frame
    );

    // Undo puts the base plan back exactly.
    c.call(&pid, "undo", serde_json::json!({})).unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(!pl.edited, "undo restores the import");
    assert_eq!(sig_of(&pl.plan), base_sig);

    // Split part 3 in the middle: same frames, one more part, still valid.
    let p3 = &pl.plan.parts[2];
    let mid = (p3.out.start() + p3.out.end()) / 2;
    c.split_at_frame(&pid, &cut, mid).unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    assert_eq!(pl.plan.parts.len(), imp.base.parts.len() + 1);
    assert_eq!(pl.plan.output.frames, 1807);
    assert!(gmclip_plan::check_plan(&pl.plan).is_empty());

    // Move the first part's screen layer 10 px right through the model.
    let p1 = &pl.plan.parts[0];
    let screen = p1
        .layers
        .iter()
        .find(|l| l.role.as_str() == "screen")
        .unwrap();
    let (x, y, w) = (screen.place.x, screen.place.y, screen.place.w);
    c.place_px(
        &pid,
        &cut,
        p1.id.as_str(),
        screen.id.as_str(),
        x + 10.0,
        y,
        w,
        None,
    )
    .unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    let moved = pl.plan.parts[0]
        .layers
        .iter()
        .find(|l| l.role.as_str() == "screen")
        .unwrap();
    assert!(
        (moved.place.x - (x + 10.0)).abs() < 1e-6,
        "{} vs {}",
        moved.place.x,
        x + 10.0
    );
    assert!((moved.place.w - w).abs() < 1e-6);
    assert!(gmclip_plan::check_plan(&pl.plan).is_empty());

    // The project reopens with its import (sidecar) and its history.
    c.close_project(&pid).unwrap();
    let home = c.dirs.library.join("Slice test.gmclip");
    c.open_home(&home).unwrap();
    let again = c.planned(&pid, &cut).unwrap();
    assert_eq!(again.plan.parts.len(), imp.base.parts.len() + 1);
    let v: Value = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(v["export"]["state"], "never");
    c.shutdown();
}

/// Lab: the preview session renders the imported cut at the displayed size, and the
/// timing feeds S2a's render-thread numbers.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + Metal + AJ's media (cargo test -- --ignored)"]
fn preview_frames_render_at_display_size() {
    use gmclip_render::export::preview::{GizmoQuad, PreviewSession};
    let Some((c, pid, cut)) = setup("preview") else {
        return;
    };
    let pl = c.planned(&pid, &cut).unwrap();
    for size in [(540u32, 960u32), (1080, 1920)] {
        let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
        let mut s = PreviewSession::open(pl.plan.clone(), &mut inputs, size).unwrap();
        assert_eq!(s.size(), size);
        let giz = [GizmoQuad {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 2.0,
            rgba: [0.0, 0.6, 1.0, 1.0],
        }];
        let mut ms = Vec::new();
        for f in 0..90u64 {
            let t = std::time::Instant::now();
            let fr = s
                .render(f, &[], if f % 2 == 0 { &giz } else { &[] })
                .unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(fr.y.len(), (size.0 * size.1) as usize);
            assert_eq!(fr.uv.len(), (size.0 * size.1 / 2) as usize);
        }
        ms.sort_by(f64::total_cmp);
        eprintln!(
            "preview {}x{}: render+readback p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms (90 sequential frames)",
            size.0, size.1, ms[45], ms[85], ms[89]
        );
        // Scrub: a far seek.
        let t = std::time::Instant::now();
        s.render(1500, &[], &[]).unwrap();
        eprintln!(
            "preview {}x{}: seek to frame 1500 {:.0} ms",
            size.0,
            size.1,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
    c.shutdown();
}

/// Lab: trim, then export through the job path, and check the file has the plan's frames.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + Metal + AJ's media (cargo test -- --ignored)"]
fn trimmed_cut_exports() {
    let Some((c, pid, cut)) = setup("export") else {
        return;
    };
    let v = c.cut_view(&pid, &cut).unwrap();
    let p2 = &v["parts"][1];
    c.trim_by(&pid, &cut, p2["id"].as_str().unwrap(), "out", -10)
        .unwrap();
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let j = jobs.start_export(&c, &pid, &cut).unwrap();
    let id = j["job"].as_str().unwrap().to_owned();
    let t0 = std::time::Instant::now();
    loop {
        let st = jobs.list().into_iter().find(|x| x.id == id).unwrap();
        if st.state == "done" {
            break;
        }
        assert!(st.state != "failed", "export failed: {:?}", st.error);
        assert!(t0.elapsed().as_secs() < 300, "export took too long");
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let ex = c.exports(&pid).unwrap();
    assert_eq!(ex.len(), 1);
    assert_eq!(ex[0].frames, 1797);
    assert!(ex[0].file.exists());
    eprintln!(
        "export: {} frames in {:.1} s → {}",
        ex[0].frames,
        ex[0].seconds,
        ex[0].file.display()
    );
    let v = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(v["export"]["state"], "fresh");
    c.shutdown();
}

/// Lab: any RenderPlan through the preview (`GMCLIP_PREVIEW_PLAN=<file>`), e.g. BENCH-60-M0.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: set GMCLIP_PREVIEW_PLAN"]
fn preview_plan_from_env() {
    use gmclip_render::export::preview::PreviewSession;
    let Some(path) = std::env::var_os("GMCLIP_PREVIEW_PLAN") else {
        return;
    };
    let plan = Arc::new(gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap());
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let t = std::time::Instant::now();
    let mut s = PreviewSession::open(plan.clone(), &mut inputs, (1080, 1920)).unwrap();
    eprintln!("open {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    let mut ms = Vec::new();
    // GMCLIP_PREVIEW_PACED=1: frames on a 30 fps schedule, as playback calls them (the
    // join preroll decodes on the idle time between frames); else back to back.
    let paced = std::env::var_os("GMCLIP_PREVIEW_PACED").is_some();
    let t0 = std::time::Instant::now();
    for f in 0..plan.output.frames {
        if paced {
            let due = std::time::Duration::from_secs_f64(f as f64 / 30.0);
            if let Some(w) = due.checked_sub(t0.elapsed()) {
                std::thread::sleep(w);
            }
        }
        let t = std::time::Instant::now();
        s.render(f, &[], &[]).unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let starts: std::collections::BTreeSet<u64> =
        plan.parts.iter().map(|p| p.out.start()).collect();
    let slow: Vec<u64> = (0..ms.len())
        .filter(|&f| ms[f] > 33.3)
        .map(|f| f as u64)
        .collect();
    let at_joins = slow.iter().filter(|f| starts.contains(f)).count();
    let mut sorted = ms.clone();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    eprintln!(
        "{} frames at 1080x1920: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms; {} over 33.3 ms, {} of them a part's first frame ({} parts); others at {:?}; total {:.1} s",
        n,
        sorted[n / 2],
        sorted[n * 95 / 100],
        sorted[n - 1],
        slow.len(),
        at_joins,
        plan.parts.len(),
        slow.iter()
            .filter(|f| !starts.contains(f))
            .take(20)
            .collect::<Vec<_>>(),
        ms.iter().sum::<f64>() / 1000.0
    );
    eprintln!(
        "slow joins: {:?}",
        slow.iter()
            .filter(|f| starts.contains(f))
            .map(|&f| (f, ms[f as usize].round()))
            .collect::<Vec<_>>()
    );
}

/// A cut made here (no import sidecar) goes to lane A's model planner; one it cannot plan
/// yet comes back as a plain `Unsupported`, never a panic.
#[test]
fn a_hand_made_cut_plans_through_the_model_planner_or_says_why() {
    let c = core("model");
    let p = c.create_project("Model").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let cut = c
        .call(&pid, "cut_create", serde_json::json!({"title": "By hand"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = c.planned(&pid, &cut).unwrap_err();
    assert_eq!(e.code, "Unsupported", "{e:?}");
    c.call(
        &pid,
        "framing_create",
        serde_json::json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    )
    .unwrap();
    let e = c.planned(&pid, &cut).unwrap_err();
    assert_eq!(e.code, "Unsupported", "{e:?}");
    assert!(e.message.contains("does not plan"), "{}", e.message);
    c.shutdown();
}
