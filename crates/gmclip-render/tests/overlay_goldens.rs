//! Regression goldens for the `/1` overlay features (decision 0014) through the compositor's
//! own overlay path ([`TextLayer::active`], the tiles the export and the preview both draw):
//! caption animation (bounce, pop, fade, keyword pop) frame by frame, hashed (FNV-1a 64 of
//! every tile's anchor, size and premultiplied bytes) with the bundled OFL face.
//!
//! Re-bless (only with a reason): `GMCLIP_BLESS=1 cargo test -p gmclip-render --test overlay_goldens`.

use std::path::PathBuf;

use gmclip_render::compositor::text_layer::TextLayer;
use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A 1080×1920 `/1` plan of 90 frames over one source, with the bundled face and the RT
/// caption style; `extra` is merged in.
fn plan(extra: Value) -> gmclip_plan::Plan {
    let font = root().join("assets/fonts/figtree/Figtree-Black.ttf");
    let mut v = json!({
        "schema": "gmclip.renderplan/1",
        "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1}, "frames": 90,
                   "video": {"codec": "h264"}, "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}},
        "background": {"kind": "colour", "rgb": "#0c1118"},
        "sources": [{"id": "s1", "path": "/nonexistent.mp4"}],
        "fonts": [{"id": "figtree", "path": font.to_string_lossy(), "sha256": "d32cf04815985f3cb0fa811236a1e130db44885ea981927a3237427320e851ef", "em_per_size": 0.8}],
        "text_styles": {"Speech": {"font": "figtree", "size": 86, "fill": "#FFFFFFFF",
            "outline": {"width": 12, "colour": "#000000FF"},
            "shadow": {"dx": 7, "dy": 7, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline"}}},
        "parts": [{"id": "p01", "out": [0, 90], "picture": {"source": "s1", "first_frame": 0},
                   "audio": {"source": "s1", "first_sample": 0, "rate": 48000}, "layers": []}],
        "text_events": [],
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    if let (Value::Object(base), Value::Object(add)) = (&mut v, extra) {
        for (k, x) in add {
            base.insert(k, x);
        }
    }
    gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap_or_else(|e| panic!("{e}"))
}

fn cue(id: &str, out: [u64; 2], text: &str, anim: Value) -> Value {
    let mut e = json!({"id": id, "out": out, "layer": 1, "role": "caption", "style": "Speech",
        "pos": {"x": 540, "y": 1065}, "align": 2, "size": 86,
        "runs": [{"text": text}, {"text": " GPU", "fill": "#FFD400FF"}]});
    if !anim.is_null() {
        e["anim"] = anim;
    }
    e
}

/// `id frame tiles fnv…` rows for each listed frame.
fn rows(name: &str, p: &gmclip_plan::Plan, frames: &[u64], out: &mut String) {
    let mut tl = TextLayer::new(p).unwrap();
    for &f in frames {
        let tiles = tl.active(p, f).unwrap();
        let h: Vec<String> = tiles
            .iter()
            .map(|t| {
                format!(
                    "{}x{}@{},{}:{:016x}",
                    t.width,
                    t.height,
                    t.x,
                    t.y,
                    t.fnv1a()
                )
            })
            .collect();
        out.push_str(&format!("{name}\t{f}\t{}\n", h.join(" ")));
    }
}

fn check(got: &str, file: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(file);
    if std::env::var_os("GMCLIP_BLESS").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).expect("committed golden table");
    let diff: Vec<String> = got
        .lines()
        .zip(want.lines())
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("  got  {g}\n  want {w}"))
        .collect();
    assert!(
        diff.is_empty() && got.lines().count() == want.lines().count(),
        "{} rows differ:\n{}",
        diff.len(),
        diff.join("\n")
    );
}

#[test]
fn caption_animation_frames() {
    let p = plan(json!({"text_events": [
        cue("bounce", [0, 20], "My PC made", json!({"kind": "bounce"})),
        cue("pop", [20, 40], "this in", json!({"kind": "pop", "pop_runs": [1]})),
        cue("fade", [40, 60], "67 seconds.", json!({"kind": "fade"})),
        cue("plain", [60, 80], "No animation", Value::Null),
    ]}));
    let mut got = String::from("# case\tframe\ttiles (w x h @ x,y : fnv1a64)\n");
    let frames = [0, 2, 4, 5, 19, 20, 22, 26, 40, 45, 59, 60];
    rows("anim", &p, &frames, &mut got);
    check(&got, "overlay_anim.tsv");

    // the shape of the bounce: 72 % < 118 % > 100 %, back to 76 % on its last frame; a
    // settled frame is the plain cue's tile (rasterised once, reused); the fade's first
    // frame is a quarter as opaque as its settled frame
    let mut tl = TextLayer::new(&p).unwrap();
    let mut at = std::collections::BTreeMap::new();
    for f in frames {
        at.insert(f, tl.active(&p, f).unwrap()[0].clone());
    }
    let w = |f: u64| at[&f].width;
    assert!(w(0) < w(4) && w(2) > w(4) && w(19) < w(4), "{at:?}");
    assert!(std::sync::Arc::ptr_eq(&at[&4], &at[&5]));
    let alpha = |f: u64| -> u64 { at[&f].rgba.chunks_exact(4).map(|px| u64::from(px[3])).sum() };
    let (a0, a5) = (alpha(40), alpha(45));
    assert!(
        a0 * 4 < a5 + a5 / 50 && a0 * 4 > a5 - a5 / 50,
        "{a0} vs {a5}"
    );
}

#[test]
fn images_and_callouts() {
    // a 64×32 test picture: a gradient with a transparent hole, stored as PNG
    let dir = std::env::temp_dir().join(format!("gmclip-overlay-goldens-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (64u32, 32u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let hole = (24..40).contains(&x) && (8..24).contains(&y);
            px.extend_from_slice(&[
                (x * 4) as u8,
                (y * 8) as u8,
                200,
                if hole { 0 } else { 255 },
            ]);
        }
    }
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: w,
        height: h,
        px,
    })
    .unwrap();
    let path = dir.join("logo.png");
    std::fs::write(&path, &png).unwrap();
    let p = plan(json!({
        "images": [{"id": "logo", "path": path.to_string_lossy(), "sha256": "ab".repeat(32), "w": 64, "h": 32}],
        "image_events": [
            {"id": "i1", "out": [0, 30], "layer": 10, "image": "logo", "place": {"x": 40.4, "y": 60, "w": 320, "h": 160}},
            {"id": "i2", "out": [10, 20], "layer": 10, "image": "logo", "place": {"x": 600, "y": 60, "w": 48, "h": 24}, "opacity": 0.5}
        ],
        "shape_events": [
            {"id": "b1", "out": [0, 30], "layer": 10, "shape": {"kind": "box", "rect": {"x": 100, "y": 400, "w": 500, "h": 260}, "width": 9, "stroke": "#FF3B30FF", "radius": 14}},
            {"id": "b2", "out": [0, 30], "layer": 5, "shape": {"kind": "box", "rect": {"x": 80, "y": 1300, "w": 920, "h": 200}, "width": 0, "fill": "#101010CC", "radius": 24}},
            {"id": "a1", "out": [0, 30], "layer": 25, "shape": {"kind": "arrow", "from": {"x": 820, "y": 900}, "to": {"x": 560, "y": 640}, "width": 12, "stroke": "#FFD400FF"}}
        ],
        "text_events": [cue("c1", [0, 30], "Look here", Value::Null)]
    }));
    let mut got = String::from("# case\tframe\ttiles (w x h @ x,y : fnv1a64)\n");
    rows("callouts", &p, &[0, 15], &mut got);
    check(&got, "overlay_shapes.tsv");
    // draw order: by layer (caption 1, panel 5, image + images + box 10, arrow 25), and at
    // one layer images before shapes before text
    let mut tl = TextLayer::new(&p).unwrap();
    let t = tl.active(&p, 15).unwrap();
    let sizes: Vec<(u32, u32)> = t.iter().map(|t| (t.width, t.height)).collect();
    assert_eq!(
        sizes,
        [
            (524, 85),
            (923, 202),
            (320, 160),
            (48, 24),
            (512, 272),
            (267, 267)
        ]
    );
    // the image's hole stays transparent and its gradient is resampled exactly
    let im = &t[2];
    let at = |x: u32, y: u32| &im.rgba[((y * im.width + x) * 4) as usize..][..4];
    assert_eq!(at(160, 80)[3], 0, "the hole");
    assert_eq!(at(0, 0), [0, 0, 200, 255]);
    // the half-opacity copy is half as opaque
    assert_eq!(t[3].rgba[3], 128);
    let _ = std::fs::remove_dir_all(&dir);
}
