//! M5 (lab): the preview session and the scrub proxy on Windows.
//!
//! - R-COMP-1: at the canvas size with no overrides, every preview frame is byte for
//!   byte the NV12 the export hands its encoder (one compose, `rcomp1_shared`), in a
//!   forward run, across joins and jumping back.
//! - The scrub proxy (H.05) builds through Media Foundation (decode, box downscale,
//!   encode) at 640 px, GOP 4; read back by frame index it matches the master
//!   downscaled; thumbnails come out; a scrub reads it.
//!
//! Lab media in `$GMCLIP_LAB_MEDIA`; needs a GPU, so ignored by default:
//!
//!   cargo test --release -p gmclip-render --test preview_mf -- --ignored --nocapture
#![cfg(windows)]

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_render::export::preview::{PreviewSession, ScrubPath};
use gmclip_render::export::proxy::{self, BuildOptions, ProxyReader};
use gmclip_render::export::{DemuxInputs, ExportOptions, export};

mod mf_lab;

use mf_lab::{FRAMES, lab, plan};

fn sha256(y: &[u8], uv: &[u8]) -> String {
    let mut all = y.to_vec();
    all.extend_from_slice(uv);
    gmclip_render::sha256::hex(&all)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()
}

#[test]
#[ignore = "lab: needs a GPU, Media Foundation and $GMCLIP_LAB_MEDIA"]
fn preview_equals_export_and_the_proxy_scrubs() {
    let Some(src) = lab() else { return };
    let plan = Arc::new(plan(&src));
    let dir = std::env::temp_dir().join(format!("gmclip-preview-mf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // the export's encoder input, per output frame
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &dir.join("ref.mp4"),
        &ExportOptions {
            hash_encoder_input: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(rep.encoder_input_hashes.len() as u64, FRAMES);

    // R-COMP-1: the preview at the canvas size is the export's frame
    let (cw, ch) = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let mut s = PreviewSession::open(plan.clone(), &mut DemuxInputs::new(None), (cw, ch)).unwrap();
    assert_eq!(s.size(), (cw, ch));
    let order: Vec<u64> = (0..FRAMES)
        .step_by(7)
        .chain([44, 45, 46, 89, 90, 91, 119, 3, 60, 59, 0])
        .collect();
    let t = std::time::Instant::now();
    for &f in &order {
        let p = s.render(f, &[], &[]).unwrap();
        assert_eq!(
            sha256(&p.y, &p.uv),
            rep.encoder_input_hashes[f as usize],
            "preview frame {f} differs from the export's"
        );
    }
    eprintln!(
        "R-COMP-1: {} preview frames byte-identical to the export's ({:.1} ms/frame)",
        order.len(),
        t.elapsed().as_secs_f64() * 1000.0 / order.len() as f64
    );

    // the scrub proxy
    let pdir = dir.join("proxy");
    let key = proxy::content_key(&src).unwrap();
    let b = proxy::build(&src, &pdir, &key, &BuildOptions::default()).unwrap();
    eprintln!(
        "proxy: {} frames at {:?} in {:.2} s ({:.0} fps), {} kB",
        b.encoded,
        b.size,
        b.seconds,
        b.encoded as f64 / b.seconds,
        b.bytes / 1000
    );
    assert_eq!((b.frames, b.encoded), (240, 240));
    assert_eq!(b.size, (640, 360));
    let mut r = ProxyReader::open(&pdir).unwrap();
    assert!(r.manifest().complete);
    assert_eq!(r.size(), (640, 360));
    // proxy frames against the master's, both thumbnailed to the same size
    let mut worst = f64::INFINITY;
    for n in [0u64, 1, 3, 4, 30, 101, 239] {
        let (pw, ph, prgb) = r.thumbnail(n, 320).unwrap();
        let (mw, mh, mrgb) = proxy::file_thumbnail(&src, n - n % 4, 320).unwrap();
        assert_eq!((pw, ph), (mw, mh));
        let p = psnr(&prgb, &mrgb);
        worst = worst.min(p);
    }
    eprintln!("proxy keyframe thumbnails vs the master's: worst PSNR {worst:.1} dB");
    assert!(worst > 30.0, "worst {worst:.1} dB");
    // every frame decodes by index (no reordering: presentation index = sample index)
    for n in [5u64, 6, 7, 120, 238, 239] {
        let f = r.frame(n).unwrap();
        assert_eq!(u64::from(f.sample.0), n % proxy::SEG_FRAMES);
    }

    // a scrub reads it, and it looks like the exact frame
    s.set_proxy_dirs(HashMap::from([(
        src.to_str().unwrap().to_owned(),
        pdir.clone(),
    )]));
    let mut paths = Vec::new();
    for f in [10u64, 50, 100] {
        let (p, path) = s.render_scrub(f, &[], &[]).unwrap();
        let exact = s.render(f, &[], &[]).unwrap();
        let q = psnr(&p.y, &exact.y);
        eprintln!("scrub frame {f}: {path:?}, Y PSNR {q:.1} dB against exact");
        assert!(q > 25.0, "scrub frame {f}: {q:.1} dB");
        paths.push(path);
    }
    assert!(paths.contains(&ScrubPath::Proxy), "{paths:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Playback cost on Windows: sequential exact frames through the preview session at a
/// viewer's size (half the canvas), ms per frame (a 30 fps plan needs < 33 ms, 60 fps
/// < 16.7 ms).
#[test]
#[ignore = "lab: needs a GPU, Media Foundation and $GMCLIP_LAB_MEDIA"]
fn playback_frame_time() {
    let Some(src) = lab() else { return };
    let plan = Arc::new(plan(&src));
    let mut s = PreviewSession::open(plan, &mut DemuxInputs::new(None), (540, 960)).unwrap();
    s.render(0, &[], &[]).unwrap();
    let mut times = Vec::new();
    for f in 1..FRAMES {
        let t = std::time::Instant::now();
        s.render(f, &[], &[]).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > 8.0 {
            eprintln!("frame {f}: {ms:.1} ms");
        }
        times.push(ms);
    }
    times.sort_by(f64::total_cmp);
    let p = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    eprintln!(
        "playback at {:?}: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms per frame",
        s.size(),
        p(0.5),
        p(0.95),
        p(1.0)
    );
}
