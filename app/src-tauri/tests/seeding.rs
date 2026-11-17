//! A2.20: 9:16 seeding from the stream layout (SPEC §5.7.1 steps 0–3, 5, 6; AJ profile, no
//! matte). A cut made from a staged loc_4fadb756 moment gets a screen layer and a face
//! layer whose crop is the stream layout's cam region, the caption seat 1065 (pinned, so
//! the framing's), no key, and zero R-LINT-1 warnings — with no profile (AJ's defaults)
//! and with AJ's profile (`lab/fixtures/profiles/aj`: its pinned seats). Lab: the lab media.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::{
    Cut, Framing, Layer, LayerRole, Layout, Matte, MemberRole, Part, RegionRole, Source,
    StreamLayout, SyncMember,
};
use gmclip_model::{Doc, Id};
use serde_json::Value;

/// loc_4fadb756's camera inset in AJ's reels: `[2060, 634, 496, 340]` of 2560×1440.
const CAM: [f64; 4] = [
    2060.0 / 2560.0,
    634.0 / 1440.0,
    496.0 / 2560.0,
    340.0 / 1440.0,
];

fn core_at(root: &Path) -> std::sync::Arc<AppCore> {
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    c.agents.start(
        &c,
        gmclip_mcp::paths::Env {
            app_data: root.join("app"),
            socket: root.join("s").join("s"),
        },
    );
    c
}

fn id<T>(s: &str) -> Id<T> {
    Id::new(gmclip_model::Ulid::parse(s).unwrap())
}

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

/// Imports the stream, stages a moment and makes a cut from it; checks the seeding.
fn seeded_cut(c: &std::sync::Arc<AppCore>) {
    let imp = gmclip_app::labfixture::open_project(c, "loc_4fadb756").unwrap();
    let pid = imp["id"].as_str().unwrap().to_owned();
    let proj = c.project(&pid).unwrap();
    c.analysis.cancel_project(&pid);

    // The import gave the stream's picture its layout: the cam box AJ's reels used.
    let doc = proj.handle.snapshot().doc.clone();
    let pic: &Source = doc
        .iter::<SyncMember>()
        .find(|m| m.role == MemberRole::Picture)
        .and_then(|m| doc.get(m.source))
        .unwrap();
    let sl: &StreamLayout = doc
        .get(pic.stream_layout.expect("a stream layout"))
        .unwrap();
    assert_eq!((sl.canvas_size.w, sl.canvas_size.h), (2560, 1440));
    let cam = sl
        .regions
        .iter()
        .find(|r| r.role == RegionRole::Cam)
        .unwrap();
    assert!(
        close([cam.rect.x, cam.rect.y, cam.rect.w, cam.rect.h], CAM),
        "{:?}",
        cam.rect
    );

    // A staged moment (stage one with a review span when the project has none here).
    let list = gmclip_app::review::moments_list(&proj, &c.transcripts);
    let ms = list["moments"].as_array().unwrap();
    let m: &Value = ms
        .iter()
        .find(|m| m["state"] == "staged" && !m["review_s"].is_null())
        .or_else(|| ms.iter().find(|m| !m["review_s"].is_null()))
        .unwrap();
    let mid = m["id"].as_str().unwrap();
    if m["state"] != "staged" {
        c.call(
            &pid,
            "moment_set_state",
            serde_json::json!({"moment": mid, "state": "staged"}),
        )
        .unwrap();
    }
    let made = gmclip_app::cutnew::from_moment(c, &pid, mid).unwrap();
    let (cut, framing) = (
        id::<Cut>(made["cut"].as_str().unwrap()),
        id::<Framing>(made["framing"].as_str().unwrap()),
    );

    let doc: Doc = proj.handle.snapshot().doc.clone();
    let fr = doc.get(framing).unwrap();
    assert_eq!((fr.canvas.w, fr.canvas.h), (1080, 1920));
    let parts: Vec<&Part> = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
        .collect();
    assert_eq!(parts.len(), 1);
    let lid = parts[0]
        .look
        .get(&framing)
        .and_then(|l| l.layout_ref)
        .unwrap();
    let layout: &Layout = doc.get(lid).unwrap();
    // step 5: AJ's seat is pinned (his profile, and the default with none), so it is the
    // framing's — the layout carries none, and "Caption height" still moves it
    assert_eq!(layout.caption_place, None);
    let seat = fr.caption_place.expect("a caption seat on the framing");
    assert!((seat.y * 1920.0 - 1065.0).abs() < 1e-6, "{seat:?}");
    let eff =
        gmclip_model::effective::effective_caption_place(&doc, parts[0].id, framing, None).unwrap();
    assert!((eff.y * 1920.0 - 1065.0).abs() < 1e-6, "{eff:?}");
    // steps 2–3: screen + face, the face from the stream layout's cam region; no key
    let layers: Vec<&Layer> = doc
        .children::<Layer>(lid.entity())
        .filter_map(|l| doc.get(l))
        .collect();
    assert_eq!(layers.len(), 2, "{layers:?}");
    let screen = layers
        .iter()
        .find(|l| l.role == LayerRole::Region(RegionRole::Screen))
        .unwrap();
    let face = layers
        .iter()
        .find(|l| l.role == LayerRole::Region(RegionRole::Cam))
        .unwrap();
    assert!(face.z > screen.z);
    assert!(
        close([face.crop.x, face.crop.y, face.crop.w, face.crop.h], CAM),
        "{:?}",
        face.crop
    );
    assert!((face.place.x * 1080.0 - 160.0).abs() < 1e-6);
    assert!((face.place.y * 1920.0 - 1120.0).abs() < 1e-6);
    assert!((face.place.w * 1080.0 - 760.0).abs() < 1e-6);
    for l in &layers {
        assert_eq!(l.matte, Matte::None, "no key on a seeded layer");
    }
    for o in parts[0]
        .look
        .get(&framing)
        .map(|l| l.overrides.values())
        .into_iter()
        .flatten()
    {
        assert!(o.matte.is_none(), "no key in a part override");
    }
    // R-LINT-1: no warning or error on the seeded cut
    let f = gmclip_core::lint::lint_cut(&doc, cut);
    let loud: Vec<_> = f
        .iter()
        .filter(|x| x.level != gmclip_core::lint::Level::Info)
        .map(|x| (&x.rule, &x.message))
        .collect();
    assert!(loud.is_empty(), "{loud:?}");
    // and it plans
    let pl = c.planned(&pid, made["cut"].as_str().unwrap()).unwrap();
    assert!(gmclip_plan::check_plan(&pl.plan).is_empty());
}

#[test]
fn a_cut_from_a_staged_moment_is_seeded_from_the_stream_layout() {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return;
    }
    let base = std::env::temp_dir().join(format!("gmclip-seeding-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    // No profile: AJ's screen + facecam and his 1065 seat are the defaults.
    let root = base.join("none");
    let c = core_at(&root);
    seeded_cut(&c);
    c.shutdown();

    // AJ's profile (a committed fixture): its pinned seats [1065, 1110].
    let root = base.join("aj");
    let c = core_at(&root);
    let dir = root.join("Profiles/aj");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        gmclip_model::lab::fixture("profiles/aj/profile.json"),
        dir.join("profile.json"),
    )
    .unwrap();
    gmclip_core::profile::register_local(&c.dirs.app_data, &dir).unwrap();
    c.agents
        .owner_call(
            "profile_use",
            &serde_json::json!({"path": dir.to_string_lossy()}),
        )
        .unwrap();
    seeded_cut(&c);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&base);
}
