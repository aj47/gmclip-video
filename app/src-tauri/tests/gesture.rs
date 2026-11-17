//! U.12 / A2.11 (R-GEST-1, R-CC-2): a human drag holds the layer and its part in the
//! core; an agent's place_set on that layer meanwhile gets Conflict{by: human}; the
//! human's own commit goes through; after the gesture ends the agent's write lands. And
//! through the viewer: a drag of 120 moves commits exactly one transaction, Esc commits
//! none. On AJ's h79 reel.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_core::{CallRequest, Caller};
use gmclip_proto::{Base, CmdError};
use serde_json::json;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn setup(tag: &str) -> Option<(Arc<AppCore>, String, String)> {
    if !Path::new(video()).is_file() {
        return None;
    }
    let root = std::env::temp_dir().join(format!("gmclip-gest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    Some((
        c,
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    ))
}

#[test]
fn an_agent_write_under_a_human_gesture_conflicts() {
    let Some((c, pid, cut)) = setup("cc") else {
        return;
    };
    let pl = c.planned(&pid, &cut).unwrap();
    let ix = &pl.index.parts[0];
    let (part, layer) = (ix.model.clone(), ix.layers[0].1.clone());
    let h = c.project(&pid).unwrap().handle.clone();
    let ent = |k: &str, id: &str| -> gmclip_model::EntityId {
        if k == "part" {
            gmclip_app::planner::ulid_id::<gmclip_model::entities::Part>(id)
                .unwrap()
                .entity()
        } else {
            gmclip_app::planner::ulid_id::<gmclip_model::entities::Layer>(id)
                .unwrap()
                .entity()
        }
    };
    let g = h
        .gesture_begin(vec![ent("part", &part), ent("layer", &layer)])
        .unwrap();
    let agent = |h: &gmclip_core::ProjectHandle, x: f64| {
        h.call(CallRequest {
            command: "place_set".into(),
            args: json!({"layer": layer, "place": {"x": x, "y": 0.0, "w": 1.0}}),
            base: Some(Base::AsOf {
                as_of: h.snapshot().doc.rev(),
            }),
            request_id: None,
            caller: Caller::agent("local:test-agent", "conn-1"),
        })
    };
    match agent(&h, 0.1) {
        Err(CmdError::Conflict(cf)) => {
            assert_eq!(cf.entities[0].by, "human");
            eprintln!("agent refused: {:?}", cf.diff);
        }
        other => panic!("expected Conflict{{by: human}}, got {other:?}"),
    }
    // the human's own write goes through while the gesture is open
    h.human(
        "place_set",
        json!({"layer": layer, "place": {"x": 0.05, "y": 0.0, "w": 1.0}}),
    )
    .unwrap();
    h.gesture_end(g);
    std::thread::sleep(Duration::from_millis(50));
    assert!(agent(&h, 0.2).is_ok(), "after the gesture the agent writes");
    c.shutdown();
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + Metal"]
fn a_drag_is_one_transaction_and_esc_is_none() {
    use gmclip_app::frame::{Record, RecordKind};
    use gmclip_app::viewer::ViewerMsg;
    let Some((c, pid, cut)) = setup("drag") else {
        return;
    };
    let slot = Arc::new(gmclip_app::frame::FrameSlot::default());
    let (_rtx, rrx) = std::sync::mpsc::channel();
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let viewer = gmclip_app::viewer::spawn(&c, Arc::clone(&slot), rrx, connected);
    let s2 = Arc::clone(&slot);
    std::thread::spawn(move || {
        loop {
            let _ = s2.take(Duration::from_millis(100));
        }
    });
    viewer.send(ViewerMsg::Load {
        project: pid.clone(),
        cut: cut.clone(),
        framing: None,
        keep: false,
    });
    std::thread::sleep(Duration::from_millis(1500));
    let rev = || c.project(&pid).unwrap().handle.snapshot().doc.rev();
    let rec = |kind: RecordKind, x: f32, y: f32| Record {
        kind,
        buttons: 1,
        mods: 0,
        pointer: 0,
        x,
        y,
        dx: 0.0,
        dy: 0.0,
        code: 0,
        t: 0.0,
    };
    // drag the full-frame layer from the centre: 120 moves, release
    let r0 = rev();
    let mut v = vec![rec(RecordKind::Down, 0.5, 0.5)];
    for i in 0..120 {
        v.push(rec(RecordKind::Move, 0.5 + i as f32 * 0.0005, 0.5));
    }
    v.push(rec(RecordKind::Up, 0.56, 0.5));
    viewer.send(ViewerMsg::Records(v));
    std::thread::sleep(Duration::from_millis(800));
    let r1 = rev();
    eprintln!("drag: rev {r0} → {r1}");
    assert_eq!(r1, r0 + 1, "a drag of 120 moves is one transaction");
    let txns_after_drag = r1;
    // Esc mid-drag: nothing is written
    let mut v = vec![rec(RecordKind::Down, 0.5, 0.5)];
    for i in 0..30 {
        v.push(rec(RecordKind::Move, 0.5 - i as f32 * 0.001, 0.5));
    }
    v.push(rec(RecordKind::Cancel, 0.0, 0.0));
    v.push(rec(RecordKind::Up, 0.47, 0.5));
    viewer.send(ViewerMsg::Records(v));
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(rev(), txns_after_drag, "Esc writes nothing");
    c.shutdown();
}

/// U.12 gizmos besides the viewer's layer boxes: the crop pane's drag, a text overlay's
/// move and the caption seat each hold their entity (an agent's write meanwhile gets
/// Conflict{by: human}), commit one transaction that ends the hold, and a hold let go
/// with no commit (Esc) writes nothing.
#[test]
fn crop_text_and_seat_drags_are_gestures() {
    let Some((c, pid, cut)) = setup("gz") else {
        return;
    };
    let h = c.project(&pid).unwrap().handle.clone();
    let rev = || h.snapshot().doc.rev();
    let agent = |command: &str, args: serde_json::Value| {
        h.call(CallRequest {
            command: command.into(),
            args,
            base: Some(Base::AsOf {
                as_of: h.snapshot().doc.rev(),
            }),
            request_id: None,
            caller: Caller::agent("local:test-agent", "conn-1"),
        })
    };
    let refused = |r: Result<gmclip_core::CallOk, CmdError>, what: &str| match r {
        Err(CmdError::Conflict(cf)) => assert_eq!(cf.entities[0].by, "human", "{what}"),
        other => panic!("{what}: expected Conflict{{by: human}}, got {other:?}"),
    };
    let framing = c.active_framing(&pid, &cut).unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    let ix = &pl.index.parts[0];
    let (part, layer) = (ix.model.clone(), ix.layers[0].1.clone());
    let begin = |kind: &str, id: &str, part: Option<&str>| {
        gmclip_app::lanes::gesture_begin_with(&c, &pid, kind, id, part).unwrap()["gesture"]
            .as_u64()
            .unwrap()
    };

    // the crop pane: the layer and its part are held; the commit is one crop_set
    let g = begin("layer", &layer, Some(&part));
    refused(
        agent(
            "crop_set",
            json!({"layer": layer, "part": part, "framing": framing,
                   "crop": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 0.5}}),
        ),
        "crop under a crop drag",
    );
    let r0 = rev();
    gmclip_app::framing::crop_norm(&c, &pid, &cut, &part, &layer, [0.1, 0.1, 0.4, 0.4]).unwrap();
    gmclip_app::lanes::gesture_end(&c, &pid, g);
    assert_eq!(rev(), r0 + 1, "a crop drag is one transaction");

    // a text hook on the frame: its box is the glyphs' (inside the canvas, in the hook band)
    gmclip_app::short::hook_set(&c, &pid, &cut, "My PC made this", None, Some(3000)).unwrap();
    let v = gmclip_app::gizmos::view(&c, &pid, &cut, 10).unwrap();
    eprintln!("gizmos: {v}");
    let t = &v["texts"][0];
    assert_eq!(t["role"], "text_hook");
    let b: Vec<f64> = t["box"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(
        b[0] >= 0.0 && b[1] >= 0.0 && b[0] + b[2] <= 1080.0 && b[1] + b[3] <= 1920.0,
        "{b:?}"
    );
    assert!(
        b[2] > 100.0 && b[3] > 30.0,
        "a box the size of a line of text: {b:?}"
    );
    let place_y = t["place"]["y"].as_f64().unwrap();
    assert!(
        b[1] < place_y * 1920.0 + 1.0 && place_y * 1920.0 - 1.0 < b[1] + b[3],
        "{b:?} vs y {place_y}"
    );
    // past the hook's end nothing is listed
    let late = gmclip_app::gizmos::view(&c, &pid, &cut, 200).unwrap();
    assert_eq!(late["texts"].as_array().unwrap().len(), 0);

    // its drag: held, one overlay_move, the hold ends with it
    let ov = t["overlay"].as_str().unwrap().to_owned();
    let g = begin("overlay", &ov, None);
    refused(
        agent(
            "overlay_move",
            json!({"overlay": ov, "place": {"x": 0.1, "y": 0.1, "w": 0.5}}),
        ),
        "move under a text drag",
    );
    let r0 = rev();
    gmclip_app::gizmos::overlay_drag(&c, &pid, &cut, &ov, (0.5, 0.2), Some(g)).unwrap();
    assert_eq!(rev(), r0 + 1, "a text drag is one transaction");
    let moved = gmclip_app::gizmos::view(&c, &pid, &cut, 10).unwrap();
    assert!((moved["texts"][0]["place"]["y"].as_f64().unwrap() - 0.2).abs() < 1e-9);
    assert!(
        (moved["texts"][0]["box"][1].as_f64().unwrap() - b[1]).abs() > 10.0,
        "the box moved with it"
    );
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        agent(
            "overlay_move",
            json!({"overlay": ov, "framing": framing, "place": {"x": 0.5, "y": 0.18, "w": 0.9}}),
        )
        .is_ok(),
        "after the commit the agent writes"
    );

    // the caption seat: the framing is held; ⇧ (one part) holds the part
    let g = begin("framing", &framing, None);
    refused(
        agent(
            "layout_edit",
            json!({"cut": cut, "ops": [{"op": "framing_set", "framing": framing,
                   "caption_place": {"x": 0.5, "y": 0.3, "w": 0.9}}]}),
        ),
        "seat under a seat drag",
    );
    let r0 = rev();
    gmclip_app::gizmos::caption_seat(&c, &pid, &cut, 1100.0, None, Some(g)).unwrap();
    assert_eq!(rev(), r0 + 1, "a seat drag is one transaction");
    let g = begin("part", &part, None);
    let r0 = rev();
    gmclip_app::gizmos::caption_seat(&c, &pid, &cut, 1300.0, Some(&part), Some(g)).unwrap();
    assert_eq!(rev(), r0 + 1, "a one-part seat drag is one transaction");
    let snap = h.snapshot();
    let pm = snap
        .doc
        .get(gmclip_app::planner::ulid_id::<gmclip_model::entities::Part>(&part).unwrap())
        .unwrap();
    let fid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Framing>(&framing).unwrap();
    let seat = pm.caption_place_override.get(&fid).copied().unwrap();
    assert!((seat.y - 1300.0 / 1920.0).abs() < 1e-9, "{seat:?}");

    // Esc: a hold let go with no commit writes nothing, and the agent is free again
    let g = begin("overlay", &ov, None);
    let r0 = rev();
    gmclip_app::lanes::gesture_end(&c, &pid, g);
    assert_eq!(rev(), r0, "Esc writes nothing");
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        agent(
            "overlay_move",
            json!({"overlay": ov, "framing": framing, "place": {"x": 0.5, "y": 0.17, "w": 0.9}}),
        )
        .is_ok()
    );
    c.shutdown();
}

/// U.12: while a text overlay (or the caption seat) is dragged, the frame itself shows the
/// move — the render thread draws those text events moved through the shared compositor
/// (`TextDrag`), nothing is written — and after the release the frame is the committed
/// plan's own: the same pixels as the preview, the same as a fresh load of that plan
/// (R-COMP-1's path, no live state left), one transaction for the drag. Esc's preview
/// goes back at once.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "lab: needs VideoToolbox + Metal"]
fn a_text_drag_previews_in_the_frame_and_the_release_is_the_plans_frame() {
    use gmclip_app::viewer::ViewerMsg;
    use gmclip_plan::types::TextRole;
    use gmclip_render::export::preview::TextShift;
    let Some((c, pid, cut)) = setup("textdrag") else {
        return;
    };
    gmclip_app::short::hook_set(&c, &pid, &cut, "My PC made this", None, Some(3000)).unwrap();
    let slot = Arc::new(gmclip_app::frame::FrameSlot::default());
    let (_rtx, rrx) = std::sync::mpsc::channel();
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let viewer = gmclip_app::viewer::spawn(&c, Arc::clone(&slot), rrx, connected);
    let last: Arc<std::sync::Mutex<Option<gmclip_app::frame::OutFrame>>> = Arc::default();
    let (s2, l2) = (Arc::clone(&slot), Arc::clone(&last));
    std::thread::spawn(move || {
        loop {
            if let Some(f) = s2.take(Duration::from_millis(100)) {
                *l2.lock().unwrap() = Some(f);
            }
        }
    });
    // the newest frame after `after` (its seq), at output frame 10
    let next = |after: u32| -> gmclip_app::frame::OutFrame {
        for _ in 0..200 {
            if let Some(f) = last.lock().unwrap().take()
                && f.seq > after
                && f.out_frame == 10
            {
                return f;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("no new frame");
    };
    let differ = |a: &gmclip_app::frame::OutFrame, b: &gmclip_app::frame::OutFrame| {
        assert_eq!((a.width, a.height), (b.width, b.height));
        a.y.iter()
            .zip(&b.y)
            .filter(|(x, y)| x.abs_diff(**y) > 2)
            .count()
    };
    viewer.send(ViewerMsg::Load {
        project: pid.clone(),
        cut: cut.clone(),
        framing: None,
        keep: false,
    });
    std::thread::sleep(Duration::from_millis(1500));
    viewer.send(ViewerMsg::Full(true));
    viewer.send(ViewerMsg::Seek(10));
    std::thread::sleep(Duration::from_millis(800));
    let f0 = next(0);
    let rev = || c.project(&pid).unwrap().handle.snapshot().doc.rev();
    let r0 = rev();
    let v = gmclip_app::gizmos::view(&c, &pid, &cut, 10).unwrap();
    let t = &v["texts"][0];
    let at = (t["at"][0].as_f64().unwrap(), t["at"][1].as_f64().unwrap());
    let (px, py) = (
        t["place"]["x"].as_f64().unwrap(),
        t["place"]["y"].as_f64().unwrap(),
    );
    let ov = t["overlay"].as_str().unwrap().to_owned();
    let dy = 300.0;
    let g =
        gmclip_app::lanes::gesture_begin_with(&c, &pid, "overlay", &ov, None).unwrap()["gesture"]
            .as_u64()
            .unwrap();
    // 60 moves, the last one 300 px down: the frame follows; nothing is written
    for i in 1..=60 {
        viewer.send(ViewerMsg::TextDrag(TextShift {
            role: TextRole::TextHook,
            at: Some(at),
            dx: 0.0,
            dy: dy * f64::from(i) / 60.0,
        }));
    }
    std::thread::sleep(Duration::from_millis(600));
    let fp = next(f0.seq);
    assert_eq!(viewer.state().text_drag, Some((0.0, dy)));
    let moved = differ(&f0, &fp);
    eprintln!(
        "{}x{}: the preview changed {moved} luma samples",
        fp.width, fp.height
    );
    assert!(moved > 2_000, "the frame shows the hook moved ({moved})");
    assert_eq!(rev(), r0, "previews are not logged");
    // release: the preview stays until the re-planned cut, which is the same frame
    viewer.send(ViewerMsg::TextDragEnd { committed: true });
    gmclip_app::gizmos::overlay_drag(&c, &pid, &cut, &ov, (px, py + dy / 1920.0), Some(g)).unwrap();
    assert_eq!(rev(), r0 + 1, "the drag is one transaction");
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        viewer.state().text_drag,
        None,
        "no live move after the release"
    );
    viewer.send(ViewerMsg::Seek(10));
    let f1 = next(fp.seq);
    let after = differ(&fp, &f1);
    eprintln!("release: {after} luma samples differ from the preview");
    assert_eq!(after, 0, "the committed plan's frame is the preview's");
    // a fresh load of the plan (no live state): the same frame again
    viewer.send(ViewerMsg::Load {
        project: pid.clone(),
        cut: cut.clone(),
        framing: None,
        keep: true,
    });
    std::thread::sleep(Duration::from_millis(1000));
    viewer.send(ViewerMsg::Seek(10));
    let f2 = next(f1.seq);
    assert_eq!(differ(&f1, &f2), 0, "R-COMP-1's path: the plan's own frame");
    // Esc: the preview goes back at once, nothing written
    let at2 = (at.0, at.1 + dy);
    viewer.send(ViewerMsg::TextDrag(TextShift {
        role: TextRole::TextHook,
        at: Some(at2),
        dx: 0.0,
        dy: -200.0,
    }));
    std::thread::sleep(Duration::from_millis(400));
    let fe = next(f2.seq);
    assert!(differ(&f2, &fe) > 2_000, "the second drag previews too");
    viewer.send(ViewerMsg::TextDragEnd { committed: false });
    std::thread::sleep(Duration::from_millis(400));
    viewer.send(ViewerMsg::Seek(10));
    let fb = next(fe.seq);
    assert_eq!(differ(&f2, &fb), 0, "Esc: back where the plan has it");
    assert_eq!(rev(), r0 + 1, "Esc writes nothing");
    c.shutdown();
}
