//! R.01 roto on real frames (lab: VideoToolbox, Metal, Apple Vision, AJ's media):
//! - Vision `.fast` (draft) and `.accurate` (export) on the h79 camera inset (AJ in front
//!   of a wall, 1440p VP9) and on full frames of the loc_086da141 proxy (AJ keyed over
//!   the desktop): masks found, timings printed for `milestones/m2/perf.md`;
//! - R-COMP-1 with mattes: h79 parts 2–3 with a roto cam (soften 2, a brush stroke on
//!   part 3) exported with the encoder input hashed; a canvas-size preview over the same
//!   track store renders 30 frames byte-identical to the export, with no draft badge;
//! - export with `draft_mattes` records the matted layers on the report.

#![cfg(target_os = "macos")]

use std::sync::Arc;
use std::time::Instant;

use gmclip_gpu::apple::{SegQuality, Segmenter};
use gmclip_plan::Plan;
use gmclip_plan::types::{LayerMatte, MatteStroke, Point, StrokeMode};
use gmclip_render::export::mattes::build_tracks;
use gmclip_render::export::preview::PreviewSession;
use gmclip_render::export::{DemuxInputs, ExportOptions, Inputs, export};
use gmclip_render::matte::store::MatteStore;
use gmclip_render::matte::{Mask, quality};
use gmclip_render::sha256;

const H79: &str = "schemas/examples/h79-parts-1-3.renderplan.json";
/// `preview/loc_086da141.proxy.mp4` in the lab media folder.
fn keyed_proxy() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("preview/loc_086da141.proxy.mp4"));
    P.as_str()
}

fn repo() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn h79() -> Option<Plan> {
    let mut plan = gmclip_plan::parse(&std::fs::read(repo().join(H79)).ok()?).ok()?;
    plan.resolve_paths(&gmclip_model::lab::root());
    plan.sources
        .iter()
        .all(|s| std::path::Path::new(&s.path).is_file())
        .then_some(plan)
}

fn roto(strokes: Option<Vec<MatteStroke>>) -> LayerMatte {
    LayerMatte {
        kind: gmclip_plan::consts::Roto,
        model: "apple_vision".into(),
        soften: Some(2.0),
        strokes,
    }
}

/// h79 with a roto cam on parts 2 and 3 (part 3 also gets a subtract stroke on the
/// inset's top-left corner).
fn h79_roto() -> Option<Plan> {
    let mut plan = h79()?;
    for p in &mut plan.parts {
        let id = p.id.as_str().to_owned();
        for l in &mut p.layers {
            if l.id.as_str() == "cam" && l.is_visible() {
                let strokes = (id.starts_with("03")).then(|| {
                    vec![MatteStroke {
                        mode: StrokeMode::Subtract,
                        radius: 30.0,
                        feather: Some(0.5),
                        points: vec![
                            Point {
                                x: 2070.0,
                                y: 640.0,
                            },
                            Point {
                                x: 2150.0,
                                y: 650.0,
                            },
                        ],
                    }]
                });
                l.matte = Some(roto(strokes));
            }
        }
    }
    Some(plan)
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + Apple Vision + AJ's media (run with --features lab)"
)]
fn vision_masks_on_real_frames_and_timings() {
    let Some(plan) = h79() else {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    };
    // the camera inset of h79 part 02, 60 frames decoded in order
    let p = &plan.parts[1];
    let mut inputs = DemuxInputs::new(None);
    let v = inputs.video(&plan, p.picture.source.as_str()).unwrap();
    assert_eq!((v.width, v.height), (2560, 1440));
    drop(v);
    let mut store = MatteStore::in_memory();
    let mut sub = plan.clone();
    sub.parts = vec![p.clone()];
    sub.parts[0].layers.retain(|l| l.id.as_str() == "cam");
    sub.parts[0].layers[0].matte = Some(roto(None));
    let t0 = Instant::now();
    let b = build_tracks(&sub, &mut inputs, &store, SegQuality::Accurate, None, None).unwrap();
    let acc_s = t0.elapsed().as_secs_f64();
    assert_eq!(b.built, 1);
    let track = store.get(&b.needs[0].key).unwrap();
    let masks: Vec<Mask> = (0..track.len()).map(|i| track.frame(i).unwrap()).collect();
    let q = quality(&masks);
    eprintln!(
        "accurate track (h79 cam inset {}x{}, {} frames): {:.1} s, {:.1} ms/frame incl. VP9 decode, {:.0} fps; {} KB coded ({} B/frame); {q:?}",
        track.width,
        track.height,
        track.len(),
        acc_s,
        acc_s * 1000.0 / track.len() as f64,
        track.len() as f64 / acc_s,
        track.bytes() / 1024,
        track.bytes() / track.len()
    );
    assert!(
        q.coverage > 0.05 && q.coverage < 0.95,
        "a person in the inset: {q:?}"
    );
    assert!(q.empty_frames == 0, "{q:?}");
    // draft vs accurate on the same frames, and Vision's own time per call
    let mut stream = {
        let v = inputs.video(&plan, p.picture.source.as_str()).unwrap();
        gmclip_render::export::mattes::open_stream(v).unwrap()
    };
    let rect = b.needs[0].rect;
    let mut fast = Segmenter::new(SegQuality::Fast).unwrap();
    let mut accurate = Segmenter::new(SegQuality::Accurate).unwrap();
    let (mut tf, mut ta, mut agree) = (Vec::new(), Vec::new(), Vec::new());
    for i in (0..60).step_by(6) {
        let pic = stream.frame(p.picture.first_frame + i).unwrap();
        let gmclip_media::decode::FrameImage::Apple(pb) = &pic.image else {
            panic!("cpu frame")
        };
        let t = Instant::now();
        let (f, _) = fast.segment_crop(pb, rect).unwrap();
        tf.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let (a, _) = accurate.segment_crop(pb, rect).unwrap();
        ta.push(t.elapsed().as_secs_f64() * 1000.0);
        let fm = Mask {
            width: f.width,
            height: f.height,
            data: f.data,
        }
        .resize(rect.2, rect.3);
        let am = Mask {
            width: a.width,
            height: a.height,
            data: a.data,
        }
        .resize(rect.2, rect.3);
        let same = fm
            .data
            .iter()
            .zip(&am.data)
            .filter(|(x, y)| (**x >= 128) == (**y >= 128))
            .count();
        agree.push(same as f64 / fm.data.len() as f64);
        if i == 0 {
            eprintln!(
                "vision output sizes: fast {}x{}, accurate {}x{}",
                f.width, f.height, a.width, a.height
            );
        }
    }
    eprintln!(
        "per call on a {}x{} crop: fast {:.1} ms (max {:.1}), accurate {:.1} ms (max {:.1}); fast/accurate agreement {:.3}",
        rect.2,
        rect.3,
        mean(&tf),
        tf.iter().cloned().fold(0.0, f64::max),
        mean(&ta),
        ta.iter().cloned().fold(0.0, f64::max),
        mean(&agree)
    );
    assert!(
        mean(&agree) > 0.85,
        "draft and export masks agree on most pixels"
    );
    store = MatteStore::in_memory();
    drop(store);

    // AJ keyed over the desktop (loc_086da141): full 720p proxy frames, `.fast`
    if std::path::Path::new(keyed_proxy()).is_file() {
        let mut ki = DemuxInputs::new(None);
        let mut kplan = plan.clone();
        kplan.sources[0].path = keyed_proxy().into();
        kplan.sources[0].fingerprint = None;
        kplan.sources[0].expect = None;
        let v = ki.video(&kplan, kplan.sources[0].id.as_str()).unwrap();
        let (w, h) = (v.width, v.height);
        let mut st = gmclip_render::export::mattes::open_stream(v).unwrap();
        let mut seg = Segmenter::sequence(SegQuality::Fast).unwrap();
        let mut ms = Vec::new();
        let t = Instant::now();
        for f in (30_000..30_060).step_by(2) {
            let pic = st.frame(f).unwrap();
            let gmclip_media::decode::FrameImage::Apple(pb) = &pic.image else {
                panic!("cpu frame")
            };
            let (m, _) = seg.segment_crop(pb, (0, 0, w, h)).unwrap();
            ms.push(Mask {
                width: m.width,
                height: m.height,
                data: m.data,
            });
        }
        let q = quality(&ms);
        eprintln!(
            "keyed cam (loc_086da141 proxy {w}x{h}, full frame, fast, 30 frames incl. decode): {:.1} ms/frame; {q:?}",
            t.elapsed().as_secs_f64() * 1000.0 / ms.len() as f64
        );
        assert!(q.coverage > 0.01, "a person over the desktop: {q:?}");
    }
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + Apple Vision + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_roto_mattes() {
    let Some(plan) = h79_roto() else {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    };
    let store = Arc::new(MatteStore::in_memory());
    let out = std::env::temp_dir().join(format!("gmclip-roto-{}.mp4", std::process::id()));
    let t0 = Instant::now();
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions {
            hash_encoder_input: true,
            mattes: Some(store.clone()),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let secs = t0.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("export.json"));
    assert!(
        rep.draft_mattes.is_empty(),
        "export waited for accurate tracks"
    );
    eprintln!("export with roto: {secs:.1} s; notes {:?}", rep.notes);
    let size = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let plan = Arc::new(plan);
    let mut s = PreviewSession::open(Arc::clone(&plan), &mut DemuxInputs::new(None), size).unwrap();
    s.set_matte_store(store.clone());
    let mut differ = Vec::new();
    let mut matted = 0;
    let n = plan.output.frames;
    for k in 0..30u64 {
        let f = 55 + k * (n - 56) / 29; // parts 2–3, where the cam is matted
        let p = s.render(f, &[], &[]).unwrap();
        assert!(
            !s.drew_draft_matte(),
            "frame {f}: the accurate track, not a draft"
        );
        matted += 1;
        let mut all = p.y;
        all.extend_from_slice(&p.uv);
        if sha256::hex(&all) != rep.encoder_input_hashes[f as usize] {
            differ.push(f);
        }
    }
    eprintln!(
        "R-COMP-1 with roto: {matted} matted frames, {} differ {differ:?}",
        differ.len()
    );
    assert!(
        differ.is_empty(),
        "preview = export with mattes: {differ:?}"
    );

    // matte view: the cam's mask in white on black, the screen gone
    s.set_matte_view(true);
    let v = s.render(100, &[], &[]).unwrap();
    let white = v.y.iter().filter(|&&y| y >= 230).count();
    let black = v.y.iter().filter(|&&y| y <= 20).count();
    let total = v.y.len();
    eprintln!("matte view: {white} white, {black} black of {total} luma samples");
    assert!(white > total / 100 && black > total / 2, "{white} {black}");
    s.set_matte_view(false);

    // without the store, the preview draws a draft and says so
    let mut d = PreviewSession::open(Arc::clone(&plan), &mut DemuxInputs::new(None), size).unwrap();
    d.render(100, &[], &[]).unwrap();
    assert!(d.drew_draft_matte());
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + Apple Vision + AJ's media (run with --features lab)"
)]
fn export_with_confirmed_draft_mattes_records_them() {
    let Some(mut plan) = h79_roto() else {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    };
    // a short cut: part 2 only
    plan.parts.retain(|p| p.id.as_str().starts_with("02"));
    let n = plan.parts[0].out.end() - plan.parts[0].out.start();
    plan.parts[0].out = gmclip_plan::scalars::Range([0, n]);
    plan.output.frames = n;
    plan.text_events = None;
    plan.fonts = None;
    plan.text_styles = None;
    let out = std::env::temp_dir().join(format!("gmclip-roto-draft-{}.mp4", std::process::id()));
    let t0 = Instant::now();
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions {
            draft_mattes: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let rec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(out.with_extension("export.json")).unwrap_or_default(),
    )
    .unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("export.json"));
    eprintln!(
        "draft-matte export ({n} frames): {:.1} s; draft_mattes {:?}",
        t0.elapsed().as_secs_f64(),
        rep.draft_mattes
    );
    assert_eq!(rep.draft_mattes, vec!["02-claim/cam".to_owned()]);
    if let Some(d) = rec.get("draft_mattes") {
        assert_eq!(d, &serde_json::json!(["02-claim/cam"]));
    }
}

/// What mattes cost an export once the tracks are built: h79 with and without the roto
/// cam, the same store (perf.md; run with gmclip-render optimised for real numbers).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + Apple Vision + AJ's media (run with --features lab)"
)]
fn export_cost_of_mattes() {
    let (Some(plain), Some(matted)) = (h79(), h79_roto()) else {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    };
    let store = Arc::new(MatteStore::in_memory());
    let t = Instant::now();
    let b = build_tracks(
        &matted,
        &mut DemuxInputs::new(None),
        &store,
        SegQuality::Accurate,
        None,
        None,
    )
    .unwrap();
    let build_s = t.elapsed().as_secs_f64();
    let run = |plan: &Plan| {
        let out = std::env::temp_dir().join(format!("gmclip-roto-cost-{}.mp4", std::process::id()));
        let t = Instant::now();
        export(
            plan,
            &mut DemuxInputs::new(None),
            &out,
            &ExportOptions {
                mattes: Some(store.clone()),
                ..ExportOptions::default()
            },
        )
        .unwrap();
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(out.with_extension("export.json"));
        t.elapsed().as_secs_f64()
    };
    let a = run(&plain);
    let m = run(&matted);
    eprintln!(
        "tracks: {} frames in {build_s:.1} s ({:.1} ms/frame); export {} frames: plain {a:.1} s, with 2 matted parts {m:.1} s (+{:.1} ms/frame over the {} matted frames)",
        b.frames,
        build_s * 1000.0 / b.frames as f64,
        plain.output.frames,
        (m - a) * 1000.0 / b.frames as f64,
        b.frames
    );
}
