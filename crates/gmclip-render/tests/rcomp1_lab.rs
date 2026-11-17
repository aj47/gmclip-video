//! H.03 / H.04 / R-COMP-1: the paused full-quality preview is the export, text included.
//! A cut is exported with the encoder's input hashed per frame; a preview session at the
//! canvas size renders 50 frames spread over the cut, and each one's NV12 must be
//! byte-identical to the export's frame. Since H.04 both go through the one per-frame
//! compose (`export::pipeline::compose`). Run on h79's example plan, and on the M0 goldens:
//! BENCH-60-M0 (41 parts, 40 discontinuous joins over the H.264 restream and the VP9 MKV)
//! and the 11 golden RT cuts converted by B-13. Lab: VideoToolbox, Metal, AJ's media and
//! the Neometric font (`--features lab`).

#![cfg(target_os = "macos")]

use std::sync::Arc;

use gmclip_plan::Plan;
use gmclip_render::export::preview::PreviewSession;
use gmclip_render::export::{DemuxInputs, ExportOptions, export};
use gmclip_render::sha256;

mod a_prime_lab;
mod common;

/// Exports `plan` hashing the encoder's input, then renders 50 frames spread over it in
/// a canvas-size preview; returns (frames with text, frames that differ).
fn preview_vs_export(name: &str, plan: Plan) -> (usize, Vec<u64>) {
    let out = std::env::temp_dir().join(format!("gmclip-rcomp1-{name}-{}.mp4", std::process::id()));
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions {
            hash_encoder_input: true,
            ..ExportOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("export.json"));
    assert_eq!(rep.encoder_input_hashes.len() as u64, plan.output.frames);
    let size = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let plan = Arc::new(plan);
    let mut s = PreviewSession::open(Arc::clone(&plan), &mut DemuxInputs::new(None), size)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(s.size(), size, "full quality: the canvas size");
    let n = plan.output.frames;
    let mut differ = Vec::new();
    let mut with_text = 0;
    for k in 0..50u64 {
        let f = k * (n - 1) / 49;
        let p = s
            .render(f, &[], &[])
            .unwrap_or_else(|e| panic!("{name} frame {f}: {e}"));
        let mut all = p.y;
        all.extend_from_slice(&p.uv);
        if sha256::hex(&all) != rep.encoder_input_hashes[f as usize] {
            differ.push(f);
        }
        if plan
            .text_events
            .as_ref()
            .is_some_and(|t| t.iter().any(|e| e.out.start() <= f && f < e.out.end()))
        {
            with_text += 1;
        }
    }
    eprintln!(
        "R-COMP-1 {name}: {} frames, {} parts; {with_text} of 50 sampled frames carry text; {} differ {differ:?}; export {:.1} s",
        n,
        plan.parts.len(),
        differ.len(),
        rep.seconds
    );
    (with_text, differ)
}

/// An example plan's font paths are as written (`fonts/…` in bench-60-m0): relative to
/// the lab media folder, where `tools/lab_media.sh` links the face.
fn resolve_fonts(plan: &mut Plan) {
    for f in plan.fonts.iter_mut().flatten() {
        if std::path::Path::new(&f.path).is_relative() {
            f.path = gmclip_model::lab::media_str(&f.path);
        }
    }
}

/// A golden cut's frozen RenderPlan (`lab/fixtures/golden-11`, written once from the
/// cut's own spec), its paths resolved against the lab media folder.
fn golden(name: &str) -> Plan {
    let p = gmclip_model::lab::fixture(format!("golden-11/{name}.renderplan.json"));
    let mut plan = gmclip_plan::parse(&std::fs::read(&p).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    plan
}

/// H.04 on the M0 goldens: BENCH-60-M0 and the 11 golden RT cuts, 50 frames each,
/// byte-identical preview and export (600 frames).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_on_the_m0_goldens() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // BENCH-60-M0 reads (a), (b) and (b)'s audio member; the golden cuts their own
    // sources. Each runs when its sources are here, and the result says which ran.
    const BENCH: [(&str, &str); 3] = [
        (
            "a",
            "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
        ),
        ("b-mkv", "streams/jMcAmooE53I-source.mkv"),
        ("b-proxy", "preview/loc_4fadb756.proxy.mp4"),
    ];
    let mut plans: Vec<(String, Plan)> = Vec::new();
    let bench_missing = gmclip_model::lab::first_missing(BENCH.map(|(_, rel)| rel));
    if let Some(gone) = &bench_missing {
        eprintln!("bench-60-m0: {}", gmclip_model::lab::skip_line(gone));
    } else {
        let mut bench = gmclip_plan::parse(
            &std::fs::read(repo.join("schemas/examples/bench-60-m0.renderplan.json")).unwrap(),
        )
        .unwrap();
        resolve_fonts(&mut bench);
        // the lab copies of the corpus sources, as export_lab's BENCH-60-M0 render reads them
        for s in &mut bench.sources {
            s.path = BENCH
                .iter()
                .find(|(id, _)| *id == s.id.as_str())
                .map(|(_, rel)| gmclip_model::lab::media_str(rel))
                .unwrap_or_else(|| panic!("source {}", s.id.as_str()));
            s.fingerprint = None;
        }
        plans.push(("bench-60-m0".into(), bench));
    }
    let goldens = common::goldens_here(|_| Vec::new());
    if bench_missing.is_some() && goldens.skip_if_empty() {
        return;
    }
    for name in goldens.items.iter().cloned() {
        let plan = golden(&name);
        plans.push((name, plan));
    }
    let mut failed = Vec::new();
    let (mut frames, mut text) = (0, 0);
    for (name, plan) in plans {
        let (t, differ) = preview_vs_export(&name, plan);
        frames += 50;
        text += t;
        if !differ.is_empty() {
            failed.push((name, differ));
        }
    }
    eprintln!(
        "R-COMP-1 M0 goldens: {frames} frames ({text} with text), {} cuts differ; BENCH-60-M0 {}; {}",
        failed.len(),
        if bench_missing.is_some() {
            "not on this machine"
        } else {
            "measured"
        },
        goldens.note("golden cuts")
    );
    assert!(failed.is_empty(), "preview ≠ export: {failed:?}");
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_text_on_50_frames() {
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    assert!(
        plan.text_events.as_ref().is_some_and(|t| t.len() > 10),
        "h79 carries text"
    );
    let out = std::env::temp_dir().join(format!("gmclip-rcomp1-{}.mp4", std::process::id()));
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions {
            hash_encoder_input: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let _ = std::fs::remove_file(&out);
    assert_eq!(rep.encoder_input_hashes.len() as u64, plan.output.frames);
    let size = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let plan = Arc::new(plan);
    let mut s = PreviewSession::open(Arc::clone(&plan), &mut DemuxInputs::new(None), size).unwrap();
    assert_eq!(s.size(), size, "full quality: the canvas size");
    let n = plan.output.frames;
    let mut differ = Vec::new();
    let mut with_text = 0;
    for k in 0..50u64 {
        let f = k * (n - 1) / 49;
        let p = s.render(f, &[], &[]).unwrap();
        let mut all = p.y;
        all.extend_from_slice(&p.uv);
        if sha256::hex(&all) != rep.encoder_input_hashes[f as usize] {
            differ.push(f);
        }
        if plan
            .text_events
            .as_ref()
            .is_some_and(|t| t.iter().any(|e| e.out.start() <= f && f < e.out.end()))
        {
            with_text += 1;
        }
    }
    eprintln!(
        "{} of 50 frames carry text; {} differ: {differ:?}",
        with_text,
        differ.len()
    );
    assert!(with_text >= 25, "most sampled frames show text");
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// At display size the text is drawn too, scaled: a half-size preview's luma matches the
/// full-size preview (= the export) box-downscaled 2×, in the hook band and overall.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn half_size_preview_draws_the_text_in_place() {
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    let (w, h) = (
        plan.output.canvas.w.get() as usize,
        plan.output.canvas.h.get() as usize,
    );
    let hook = plan
        .text_events
        .as_ref()
        .and_then(|t| t.iter().find(|e| format!("{:?}", e.role).contains("Hook")))
        .expect("h79 has a text hook");
    let f = (hook.out.start() + hook.out.end()) / 2;
    let plan = Arc::new(plan);
    let mut full = PreviewSession::open(
        Arc::clone(&plan),
        &mut DemuxInputs::new(None),
        (w as u32, h as u32),
    )
    .unwrap();
    let a = full.render(f, &[], &[]).unwrap();
    let mut half = PreviewSession::open(
        Arc::clone(&plan),
        &mut DemuxInputs::new(None),
        (w as u32 / 2, h as u32 / 2),
    )
    .unwrap();
    let b = half.render(f, &[], &[]).unwrap();
    assert_eq!((b.width as usize, b.height as usize), (w / 2, h / 2));
    let down = |x: usize, y: usize| -> f64 {
        let at = |xx: usize, yy: usize| f64::from(a.y[yy * w + xx]);
        (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1))
            / 4.0
    };
    let mae = |y0: usize, y1: usize| -> f64 {
        let mut s = 0.0;
        let mut n = 0.0;
        for y in y0..y1 {
            for x in 0..w / 2 {
                s += (down(x, y) - f64::from(b.y[y * (w / 2) + x])).abs();
                n += 1.0;
            }
        }
        s / n
    };
    // the hook band (y 220–400 on the canvas)
    let band = mae(110, 200);
    let all = mae(0, h / 2);
    eprintln!("half-size vs downscaled full: hook band MAE {band:.2}, frame MAE {all:.2}");
    assert!(band < 6.0 && all < 6.0, "band {band:.2} all {all:.2}");
}

/// R-COMP-1 with the `/1` looks (decision 0014): h79 with the blurred-source fill, word
/// bounce and keyword pop on every caption, an image and two callouts — the paused
/// full-quality preview is still the export, byte for byte, on 50 frames.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_the_v1_looks() {
    use gmclip_plan::consts::SchemaVersion;
    use gmclip_plan::types as rp;
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gmclip-rcomp1-v1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (200u32, 100u32);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            [
                (i % w) as u8,
                (i / w * 2) as u8,
                180,
                if i % 7 == 0 { 90 } else { 255 },
            ]
        })
        .collect();
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: w,
        height: h,
        px,
    })
    .unwrap();
    let img = dir.join("logo.png");
    std::fs::write(&img, png).unwrap();
    let n = plan.output.frames;
    plan.schema = SchemaVersion::V1;
    plan.background = rp::Background::BlurredSource {
        rgb: gmclip_plan::scalars::Rgb::new("#0c1118").unwrap(),
        blur: 24,
        dim: 0.3,
    };
    for e in plan.text_events.iter_mut().flatten() {
        if e.role == rp::TextRole::Caption {
            let pops: Vec<u64> = (0..e.runs.len() as u64)
                .filter(|&i| e.runs[i as usize].fill.is_some())
                .collect();
            e.anim = Some(rp::TextAnim {
                kind: Some(rp::AnimKind::Bounce),
                pop_runs: (!pops.is_empty()).then_some(pops),
            });
        }
    }
    let id = |s: &str| gmclip_plan::scalars::Id::new(s).unwrap();
    let rgba = |s: &str| gmclip_plan::scalars::Rgba::new(s).unwrap();
    plan.images = Some(vec![rp::Image {
        id: id("logo"),
        path: img.to_string_lossy().into_owned(),
        sha256: gmclip_plan::scalars::Sha256::new("ab".repeat(32)).unwrap(),
        w: u64::from(w),
        h: u64::from(h),
    }]);
    plan.image_events = Some(vec![rp::ImageEvent {
        id: id("i1"),
        out: gmclip_plan::scalars::Range([0, n / 2]),
        layer: Some(10),
        image: id("logo"),
        place: rp::Rect {
            x: 700.5,
            y: 1500.0,
            w: 300.0,
            h: 150.0,
        },
        opacity: Some(0.8),
        provenance: None,
        z: None,
    }]);
    plan.shape_events = Some(vec![
        rp::ShapeEvent {
            id: id("box"),
            out: gmclip_plan::scalars::Range([0, n]),
            layer: Some(25),
            shape: rp::Shape::Box {
                rect: rp::Rect {
                    x: 120.0,
                    y: 420.0,
                    w: 520.0,
                    h: 260.0,
                },
                width: 8.0,
                stroke: Some(rgba("#FF3B30FF")),
                fill: None,
                radius: Some(14.0),
            },
            provenance: None,
        },
        rp::ShapeEvent {
            id: id("arrow"),
            out: gmclip_plan::scalars::Range([n / 3, n]),
            layer: Some(25),
            shape: rp::Shape::Arrow {
                from: rp::Point { x: 860.0, y: 900.0 },
                to: rp::Point { x: 620.0, y: 660.0 },
                width: 12.0,
                stroke: rgba("#FFD400FF"),
                head: None,
            },
            provenance: None,
        },
    ]);
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );
    let (with_text, differ) = preview_vs_export("h79-v1-looks", plan);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(with_text >= 25);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// R-COMP-1 with cover layers (decision 0021): h79 with a cover over a region of every
/// part's screen — blur, pixelate and solid in turn, the camera above them — so the paused
/// full-quality preview is still the export, byte for byte, on 50 frames.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_cover_layers() {
    use gmclip_plan::consts::SchemaVersion;
    use gmclip_plan::types as rp;
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    plan.schema = SchemaVersion::V1;
    let mut covered = 0;
    for (k, part) in plan.parts.iter_mut().enumerate() {
        let Some(screen) = part.layers.iter().find(|l| l.role.as_str() == "screen") else {
            continue;
        };
        // the screen's top-right quarter, where it lands on the canvas
        let (c, p) = (screen.crop, screen.place);
        let crop = rp::Rect {
            x: c.x + c.w / 2.0,
            y: c.y,
            w: c.w / 2.0,
            h: c.h / 4.0,
        };
        let place = rp::Rect {
            x: p.x + p.w / 2.0,
            y: p.y,
            w: p.w / 2.0,
            h: p.h / 4.0,
        };
        let mut cover = screen.clone();
        cover.id = gmclip_plan::scalars::Id::new(format!("cover{k}")).unwrap();
        cover.role = gmclip_plan::scalars::RoleName::new("chat").unwrap();
        cover.crop = crop;
        cover.place = place;
        cover.z = screen.z + 1;
        cover.matte = None;
        cover.cover = Some(match k % 3 {
            0 => rp::LayerCover::Blur { radius: 18.0 },
            1 => rp::LayerCover::Pixelate { block: 22.5 },
            _ => rp::LayerCover::Solid {
                rgb: gmclip_plan::scalars::Rgb::new("#0c1118").unwrap(),
            },
        });
        part.layers.push(cover);
        covered += 1;
    }
    assert!(covered >= 3);
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );
    let (_, differ) = preview_vs_export("h79-covers", plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0033: h79 v4 with a blur, pixelate or solid cover on every part's screen,
/// each pinned to a point drifting along a sine (keys every 3 frames; moves rounded to
/// whole pixels per frame, the crop moved by the same amount in source pixels): 50
/// sampled frames, preview = export.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_pinned_covers() {
    use gmclip_plan::consts::SchemaVersion;
    use gmclip_plan::types as rp;
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    plan.schema = SchemaVersion::V1;
    let mut pinned = 0;
    for (k, part) in plan.parts.iter_mut().enumerate() {
        let Some(screen) = part.layers.iter().find(|l| l.role.as_str() == "screen") else {
            continue;
        };
        // a quarter-size cover in the middle of the screen
        let (c, p) = (screen.crop, screen.place);
        let mut cover = screen.clone();
        cover.id = gmclip_plan::scalars::Id::new(format!("cover{k}")).unwrap();
        cover.role = gmclip_plan::scalars::RoleName::new("private").unwrap();
        cover.crop = rp::Rect {
            x: c.x + c.w * 3.0 / 8.0,
            y: c.y + c.h * 3.0 / 8.0,
            w: c.w / 4.0,
            h: c.h / 4.0,
        };
        cover.place = rp::Rect {
            x: p.x + p.w * 3.0 / 8.0,
            y: p.y + p.h * 3.0 / 8.0,
            w: p.w / 4.0,
            h: p.h / 4.0,
        };
        cover.z = screen.z + 1;
        cover.matte = None;
        cover.ease = None;
        cover.cover = Some(match k % 3 {
            0 => rp::LayerCover::Blur { radius: 18.0 },
            1 => rp::LayerCover::Pixelate { block: 22.5 },
            _ => rp::LayerCover::Solid {
                rgb: gmclip_plan::scalars::Rgb::new("#0c1118").unwrap(),
            },
        });
        let (x0, y0) = (
            cover.place.x + cover.place.w / 2.0,
            cover.place.y + cover.place.h / 2.0,
        );
        let (a, z) = (part.out.start(), part.out.end());
        let keys: Vec<rp::TrackKey> = (a..z)
            .step_by(3)
            .map(|f| {
                let t = (f - a) as f64 / 30.0;
                rp::TrackKey {
                    frame: f,
                    x: x0 + 60.0 * (t * 1.3).sin() + 7.3 * t,
                    y: y0 + 25.0 * (t * 0.7).sin(),
                }
            })
            .collect();
        cover.track = Some(rp::LayerTrack { x: x0, y: y0, keys });
        part.layers.push(cover);
        pinned += 1;
    }
    assert!(pinned >= 3);
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );
    let (_, differ) = preview_vs_export("h79-pinned-covers", plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// R-COMP-1 on H.264 with B-frames, where `FrameStream` keeps its look-ahead topped up
/// and waits for single pictures (lane VT): (a′) under native-mode stretched to 60 s,
/// played through the preview session the way the viewer plays it (runs in order, across
/// the held part and the jump, then back and forth), every frame byte-identical to the
/// export's encoder input.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + (a′) (run with --features lab)"
)]
fn preview_equals_export_playing_a_prime() {
    let Some(src) = a_prime_lab::a_prime() else {
        return;
    };
    let plan = a_prime_lab::plan(&src);
    let out = std::env::temp_dir().join(format!("gmclip-rcomp1-aprime-{}.mp4", std::process::id()));
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions {
            hash_encoder_input: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("export.json"));
    let size = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let plan = Arc::new(plan);
    let mut s = PreviewSession::open(Arc::clone(&plan), &mut DemuxInputs::new(None), size).unwrap();
    let order: Vec<u64> = (0..240)
        .chain(700..800)
        .chain(1000..1120)
        .chain([1500, 1501, 1502, 400, 401, 1799, 0])
        .chain((0..50).map(|k| k * 1799 / 49))
        .collect();
    let mut differ = Vec::new();
    for &f in &order {
        let p = s.render(f, &[], &[]).unwrap();
        let mut all = p.y;
        all.extend_from_slice(&p.uv);
        if sha256::hex(&all) != rep.encoder_input_hashes[f as usize] {
            differ.push(f);
        }
    }
    eprintln!(
        "R-COMP-1 (a′) playing: {} preview frames, {} differ {differ:?}",
        order.len(),
        differ.len()
    );
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0028: h79 v4 over a looping backdrop video (a 1080p render, 7 s loop, on the
/// output clock across every join), with two clip events picture-in-picture (one from the
/// same render, one from h79's own 1440p source at another time, read on a stream of its
/// own), then over a still backdrop image: 50 sampled frames each, preview = export.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_backdrop_and_clips() {
    use gmclip_plan::consts::SchemaVersion;
    use gmclip_plan::types as rp;
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    let render = gmclip_model::lab::media_str("renders/own-vm-1080.mp4");
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
        || !std::path::Path::new(&render).is_file()
    {
        eprintln!("skipped: h79's sources or the backdrop render are not on this machine");
        return;
    }
    plan.schema = SchemaVersion::V1;
    let mut v = serde_json::to_value(&plan).unwrap();
    v["sources"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"id": "bd", "path": render}));
    // cover: the 16:9 render's centre 9:16 strip fills the canvas
    v["background"] = serde_json::json!({"kind": "backdrop", "rgb": "#0c1118",
        "crop": {"x": 656.25, "y": 0, "w": 607.5, "h": 1080}, "place": {"x": 0, "y": 0, "w": 1080, "h": 1920},
        "video": {"source": "bd", "first_frame": 120, "frames": 210, "loop": true}});
    let frames = plan.output.frames;
    v["clip_events"] = serde_json::json!([
        {"id": "pip1", "out": [30, 400], "layer": 1,
         "picture": {"source": "bd", "first_frame": 900, "frames": 150, "loop": true},
         "crop": {"x": 0, "y": 0, "w": 1920, "h": 1080}, "place": {"x": 560, "y": 1300, "w": 480, "h": 270}},
        {"id": "pip2", "out": [300, frames - 10], "opacity": 0.8,
         "picture": {"source": "mkv1440", "first_frame": 2000},
         "crop": {"x": 0, "y": 0, "w": 2560, "h": 1440}, "place": {"x": 40, "y": 1500.5, "w": 400, "h": 225}}
    ]);
    let video_plan: rp::Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let (_, differ) = preview_vs_export("h79-backdrop-clips", video_plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");

    // a still backdrop: a 1200×1600 gradient PNG, cover-cropped to the canvas
    let dir = std::env::temp_dir().join(format!("gmclip-rcomp1-still-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (1200u32, 1600u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x + y) % 256) as u8,
                255,
            ]);
        }
    }
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: w,
        height: h,
        px,
    })
    .unwrap();
    let png_path = dir.join("still.png");
    std::fs::write(&png_path, &png).unwrap();
    v["images"] = serde_json::json!([{"id": "still", "path": png_path.display().to_string(),
        "sha256": sha256::hex(&png), "w": w, "h": h}]);
    v["background"] = serde_json::json!({"kind": "backdrop", "rgb": "#0c1118",
        "crop": {"x": 150, "y": 0, "w": 900, "h": 1600}, "place": {"x": 0, "y": 0, "w": 1080, "h": 1920},
        "image": "still"});
    let still_plan: rp::Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let (_, differ) = preview_vs_export("h79-still-backdrop", still_plan);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0029: h79 v4 over a three-stop plate (the gradient tile under every layer):
/// 50 sampled frames, preview = export.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_a_plate() {
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    plan.background = gmclip_plan::types::Background::Plate {
        stops: ["#0a2a6b", "#1460d6", "#0a2a6b"]
            .iter()
            .map(|c| gmclip_plan::scalars::Rgb::new(*c).unwrap())
            .collect(),
    };
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    let (_, differ) = preview_vs_export("h79-plate", plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0030: h79 v4 with parts at 2×, ½× (sound stretched) and 3× (muted), captions
/// left out (their frames are the 1× cut's): the export verifies (the stretched mix is
/// what the AAC carries) and 50 sampled frames of the preview equal it.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_speed_changes() {
    use gmclip_plan::types as rp;
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    plan.schema = gmclip_plan::consts::SchemaVersion::V1;
    plan.text_events = None;
    plan.text_styles = None;
    plan.fonts = None;
    plan.audio.envelopes = None;
    let speeds: [(usize, u64, u64, Option<rp::SpeedAudio>); 3] = [
        (2, 2, 1, None),
        (5, 1, 2, None),
        (8, 3, 1, Some(rp::SpeedAudio::Mute)),
    ];
    let mut at = 0u64;
    for (i, p) in plan.parts.iter_mut().enumerate() {
        let len = p.out.len();
        let len = match speeds.iter().find(|s| s.0 == i) {
            Some(&(_, num, den, audio)) => {
                p.speed = Some(rp::PartSpeed {
                    num,
                    den,
                    audio,
                    ramp: None,
                });
                (len * den / num).max(1)
            }
            None => len,
        };
        p.out = gmclip_plan::scalars::Range([at, at + len]);
        at += len;
    }
    plan.output.frames = at;
    // a join touching a sped part is not continuous: give those sides their de-click
    let n = plan.parts.len();
    for i in 0..n {
        let fade = rp::Fade {
            samples: 144,
            shape: rp::FadeShape::Linear,
        };
        let cont_in = i > 0 && gmclip_plan::validate::continuous_join(&plan, i - 1);
        let cont_out = i + 1 < n && gmclip_plan::validate::continuous_join(&plan, i);
        plan.parts[i].audio.fade_in = Some((!cont_in).then_some(fade));
        plan.parts[i].audio.fade_out = Some((!cont_out).then_some(fade));
    }
    let errs = gmclip_plan::check_plan(&plan);
    assert!(errs.is_empty(), "{errs:?}");
    let (_, differ) = preview_vs_export("h79-speed", plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0033: h79 v4 with three parts ramping — 1×→2× linear, 2×→½× ease in-out
/// (stretched), 1×→3× ease-out (muted) — over a quarter of each part from an eighth in;
/// each part lasts until its integral reaches its source. The export verifies and 50
/// sampled frames match the paused preview.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_speed_ramps() {
    use gmclip_plan::types as rp;
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    plan.schema = gmclip_plan::consts::SchemaVersion::V1;
    plan.text_events = None;
    plan.text_styles = None;
    plan.fonts = None;
    plan.audio.envelopes = None;
    type Ramp = (
        usize,
        (u64, u64),
        (u64, u64),
        rp::EaseCurve,
        Option<rp::SpeedAudio>,
    );
    let ramps: [Ramp; 3] = [
        (2, (1, 1), (2, 1), rp::EaseCurve::Linear, None),
        (5, (2, 1), (1, 2), rp::EaseCurve::EaseInOut, None),
        (
            8,
            (1, 1),
            (3, 1),
            rp::EaseCurve::EaseOut,
            Some(rp::SpeedAudio::Mute),
        ),
    ];
    let mut at = 0u64;
    for (i, p) in plan.parts.iter_mut().enumerate() {
        let len = p.out.len();
        let len = match ramps.iter().find(|r| r.0 == i) {
            Some(&(_, a, b, curve, audio)) => {
                let sp = rp::PartSpeed {
                    num: a.0,
                    den: a.1,
                    audio,
                    ramp: Some(rp::SpeedRamp {
                        to: rp::Rational { num: b.0, den: b.1 },
                        from: len / 8,
                        frames: (len / 4).max(1),
                        curve,
                    }),
                };
                p.speed = Some(sp);
                let mut l = len / 8 + (len / 4).max(1);
                while sp.source_frames(l).unwrap() < len {
                    l += 1;
                }
                l
            }
            None => len,
        };
        p.out = gmclip_plan::scalars::Range([at, at + len]);
        at += len;
    }
    plan.output.frames = at;
    // a join touching a sped part is not continuous: give those sides their de-click
    let n = plan.parts.len();
    for i in 0..n {
        let fade = rp::Fade {
            samples: 144,
            shape: rp::FadeShape::Linear,
        };
        let cont_in = i > 0 && gmclip_plan::validate::continuous_join(&plan, i - 1);
        let cont_out = i + 1 < n && gmclip_plan::validate::continuous_join(&plan, i);
        plan.parts[i].audio.fade_in = Some((!cont_in).then_some(fade));
        plan.parts[i].audio.fade_out = Some((!cont_out).then_some(fade));
    }
    let errs = gmclip_plan::check_plan(&plan);
    assert!(errs.is_empty(), "{errs:?}");
    let (_, differ) = preview_vs_export("h79-ramp", plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0031: h79 v4 with a callout and a caption-like label pinned to moving points
/// (keys every 3 frames, the move rounded to whole pixels per frame): 50 sampled frames,
/// preview = export.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_equals_export_with_pinned_overlays() {
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    let mut v = serde_json::to_value(&plan).unwrap();
    let lab = gmclip_model::lab::root();
    for s in v["sources"].as_array_mut().unwrap() {
        let p = s["path"].as_str().unwrap().to_owned();
        if !p.starts_with('/') {
            s["path"] = serde_json::json!(lab.join(p).display().to_string());
        }
    }
    for f in v["fonts"].as_array_mut().into_iter().flatten() {
        let p = f["path"].as_str().unwrap().to_owned();
        if !p.starts_with('/') {
            f["path"] = serde_json::json!(lab.join(p).display().to_string());
        }
    }
    if v["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| !std::path::Path::new(s["path"].as_str().unwrap()).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    v["schema"] = serde_json::json!("gmclip.renderplan/1");
    let frames = v["output"]["frames"].as_u64().unwrap();
    v["shape_events"] = serde_json::json!([
        {"id": "ring", "out": [0, frames], "layer": 30, "shape": {"kind": "box", "rect": {"x": 300, "y": 600, "w": 240, "h": 140}, "width": 9, "stroke": "#FFD400FF", "radius": 16}}
    ]);
    let ring_keys: Vec<_> = (0..frames)
        .step_by(3)
        .map(|f| {
            let t = f as f64 / 30.0;
            serde_json::json!({"frame": f, "x": 420.0 + 180.0 * (t * 0.9).sin(), "y": 670.0 + 90.0 * (t * 1.3).cos()})
        })
        .collect();
    let first = v["text_events"][0].clone();
    let (a, z) = (
        first["out"][0].as_u64().unwrap(),
        first["out"][1].as_u64().unwrap(),
    );
    let label_keys: Vec<_> = (a..z)
        .step_by(2)
        .map(|f| serde_json::json!({"frame": f, "x": 540.0 + (f - a) as f64 * 1.7, "y": 900.0}))
        .collect();
    v["tracks"] = serde_json::json!([
        {"kind": "shape", "event": "ring", "keys": ring_keys},
        {"kind": "text", "event": first["id"], "keys": label_keys}
    ]);
    let plan: gmclip_plan::Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let (_, differ) = preview_vs_export("h79-pinned", plan);
    assert!(differ.is_empty(), "preview ≠ export on frames {differ:?}");
}

/// Decision 0028: a preview whose plan gains a clip event of a source it already shows
/// opens that clip's own decode stream (same files, new stream), and draws it.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn preview_takes_a_new_clip_of_an_open_source() {
    let path = format!(
        "{}/../../schemas/examples/h79-v4.renderplan.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if plan
        .sources
        .iter()
        .any(|s| !std::path::Path::new(&s.path).is_file())
    {
        eprintln!("skipped: h79's sources are not on this machine");
        return;
    }
    let size = (540, 960);
    let mut s =
        PreviewSession::open(Arc::new(plan.clone()), &mut DemuxInputs::new(None), size).unwrap();
    let before = s.render(100, &[], &[]).unwrap();
    let mut v = serde_json::to_value(&plan).unwrap();
    v["schema"] = serde_json::json!("gmclip.renderplan/1");
    v["clip_events"] = serde_json::json!([{"id": "pip", "out": [90, 120],
        "picture": {"source": "mkv1440", "first_frame": 3000},
        "crop": {"x": 0, "y": 0, "w": 2560, "h": 1440}, "place": {"x": 40, "y": 300, "w": 480, "h": 270}}]);
    let with_clip: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    s.set_plan(Arc::new(with_clip), &mut DemuxInputs::new(None))
        .unwrap();
    let after = s.render(100, &[], &[]).unwrap();
    assert_ne!(before.y, after.y, "the clip is drawn");
}
