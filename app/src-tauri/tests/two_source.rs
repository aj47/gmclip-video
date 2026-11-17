//! "Are these the same recording?" (U.15/A2.12): AJ's loc_4fadb756 stream, the 48 kHz
//! proxy (voice, transcript clock) paired with the 1440p VP9 MKV (picture) at RT's
//! measured offset (MKV frame = proxy frame − 1642, i.e. +54.733 s). The cut's part is on
//! the sync group: picture frames from the MKV, sound from the proxy; the offset can be
//! nudged later (undoable); the group is heard once, its words owned by the group. Lab:
//! a 3 s export from the pair. Skips without AJ's files.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::video::HiRes;

/// `preview/loc_4fadb756.proxy.mp4` in the lab media folder.
fn proxy() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("preview/loc_4fadb756.proxy.mp4"));
    P.as_str()
}
/// `streams/jMcAmooE53I-source.mkv` in the lab media folder.
fn mkv() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("streams/jMcAmooE53I-source.mkv"));
    P.as_str()
}
const OFFSET_S: f64 = 1642.0 / 30.0;
const F: i64 = 705_600_000;

#[test]
fn a_proxy_and_its_1440p_picture_are_one_recording() {
    if !Path::new(proxy()).is_file() || !Path::new(mkv()).is_file() {
        eprintln!("skipped: AJ's loc_4fadb756 files are not here");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-two-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_stream(
        &c,
        Path::new(proxy()),
        None,
        Some(HiRes {
            path: mkv().into(),
            offset_s: OFFSET_S,
            segments: vec![],
        }),
    )
    .unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    // the group is the analysis owner (one transcript for the pair)
    let st = gmclip_app::analysis::status(&c, &pid);
    assert_eq!(st["sources"].as_array().unwrap().len(), 1, "{st}");
    c.analysis.shutdown(Duration::from_secs(20));

    // trim to RT's verified window 1196–1199 s (both files cover it)
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(&cut).unwrap();
    let part = gmclip_model::clock::cut_layout(&doc, cid)[0].part;
    c.call(
        &pid,
        "part_trim",
        serde_json::json!({"part": part.0.to_string(), "src_in": 1196 * F, "src_out": 1199 * F}),
    )
    .unwrap();
    let pl = c.planned(&pid, &cut).unwrap().plan;
    let pp = &pl.parts[0];
    let src = |id: &str| {
        pl.sources
            .iter()
            .find(|s| s.id.as_str() == id)
            .unwrap()
            .path
            .clone()
    };
    // the app keeps a file's real path (the lab media folder holds symlinks)
    let real = |p: &str| {
        std::fs::canonicalize(p)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(
        real(&src(pp.picture.source.as_str())),
        real(mkv()),
        "picture from the 1440p file"
    );
    assert_eq!(
        real(&src(pp.audio.source.as_str())),
        real(proxy()),
        "sound from the proxy"
    );
    assert_eq!(
        pp.picture.first_frame,
        1196 * 30 - 1642,
        "the measured MKV frame (34238)"
    );
    assert_eq!(pp.audio.first_sample, 1196 * 48_000);
    assert_eq!(
        pl.output.canvas.w,
        gmclip_plan::scalars::Even::new(1920).unwrap(),
        "the canvas follows the hi-res picture"
    );
    // the offset can be nudged (one frame later), undoably
    assert!(
        (gmclip_app::video::picture_offset(&c, &pid, &cut)
            .unwrap()
            .unwrap()
            .1
            - OFFSET_S)
            .abs()
            < 1e-6
    );
    gmclip_app::video::set_picture_offset(&c, &pid, &cut, OFFSET_S - 1.0 / 30.0, None, false)
        .unwrap();
    let pl2 = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(pl2.parts[0].picture.first_frame, 1196 * 30 - 1642 + 1);
    c.call(&pid, "undo", serde_json::json!({})).unwrap();
    assert_eq!(
        c.planned(&pid, &cut).unwrap().plan.parts[0]
            .picture
            .first_frame,
        1196 * 30 - 1642
    );

    // "Offset changes here" at 1.5 s: the part splits there and the second half's picture
    // is a frame later, one undo step puts both back
    gmclip_app::video::set_picture_offset(&c, &pid, &cut, OFFSET_S - 1.0 / 30.0, Some(45), true)
        .unwrap();
    let pl3 = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(pl3.parts.len(), 2);
    assert_eq!(pl3.parts[0].picture.first_frame, 1196 * 30 - 1642);
    assert_eq!(pl3.parts[1].picture.first_frame, 1196 * 30 + 45 - 1642 + 1);
    c.call(&pid, "undo", serde_json::json!({})).unwrap();
    let pl4 = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(pl4.parts.len(), 1, "one undo step");
    assert_eq!(pl4.parts[0].picture.first_frame, 1196 * 30 - 1642);

    // h holds the hi-res screen on a paired cut (the picture member's frame)
    let h = gmclip_app::tighten::hold_toggle(&c, &pid, &cut, 30).unwrap();
    assert_eq!(h["held"], true);
    assert!(c.planned(&pid, &cut).is_ok(), "a hold on the group plans");
    gmclip_app::tighten::hold_toggle(&c, &pid, &cut, 30).unwrap();
    // lab: a 3 s export from the pair
    if std::env::var_os("GMCLIP_LAB").is_some() {
        let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
        let id = jobs.start_export(&c, &pid, &cut).unwrap()["job"]
            .as_str()
            .unwrap()
            .to_owned();
        loop {
            let s = jobs.list().into_iter().find(|x| x.id == id).unwrap();
            if s.state == "done" {
                break;
            }
            assert!(s.state != "failed", "export failed: {:?}", s.error);
            std::thread::sleep(Duration::from_millis(250));
        }
        let ex = c.exports(&pid).unwrap();
        eprintln!("export: {} frames → {}", ex[0].frames, ex[0].file.display());
        assert_eq!(ex[0].frames, 90);
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// RT's piecewise offset for this stream: −503 frames until 1000 s, −1642 after. The cut
/// is seeded as one part per stretch (a part may not straddle an offset change) and both
/// of RT's verified windows come out on their measured MKV frames.
#[test]
fn piecewise_offsets_match_rts_windows() {
    if !Path::new(proxy()).is_file() || !Path::new(mkv()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-two-pw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_stream(
        &c,
        Path::new(proxy()),
        None,
        Some(HiRes {
            path: mkv().into(),
            offset_s: 0.0,
            segments: vec![(0.0, 503.0 / 30.0), (1000.0, 1642.0 / 30.0)],
        }),
    )
    .unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.shutdown(Duration::from_secs(20));
    let pl = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(pl.parts.len(), 2, "one part per stretch");
    // a frame of each stretch: RT's windows at 195 s (MKV 5347) and 1196 s (MKV 34238)
    let first = |t_s: i64| {
        let p = pl
            .parts
            .iter()
            .find(|p| {
                let a = p.audio.first_sample as i64 / 48_000;
                let b = a + p.out.len() as i64 / 30;
                a <= t_s && t_s < b
            })
            .unwrap();
        p.picture.first_frame as i64 + (t_s * 30 - (p.audio.first_sample as i64 / 1600))
    };
    assert_eq!(first(195), 5347);
    assert_eq!(first(1196), 34238);
    let pv = gmclip_app::video::picture_offsets_view(&c, &pid, &cut).unwrap();
    assert_eq!(pv.as_array().unwrap().len(), 2, "{pv}");
    // nudge the second stretch only
    let k = pv[1]["from_frame"].as_i64().unwrap() + 30;
    gmclip_app::video::set_picture_offset(&c, &pid, &cut, 1641.0 / 30.0, Some(k), false).unwrap();
    let pl2 = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(
        pl2.parts[0].picture.first_frame,
        pl.parts[0].picture.first_frame
    );
    assert_eq!(
        pl2.parts[1].picture.first_frame,
        pl.parts[1].picture.first_frame + 1
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
