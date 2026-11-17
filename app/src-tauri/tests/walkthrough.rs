//! The M2 walkthrough on loc_4fadb756 (coordinator 2026-10-15): reproduce AJ's shipped
//! h79 v4 reel ("My PC made this in 67 seconds", 60.2 s) by hand-driving the same app
//! functions the UI's buttons call, then compare the export with the reference MP4. No real model
//! runs (the moment comes from the stream's project fixture; nothing is asked of an agent), and
//! Send to Clippy goes to a local stand-in, never AJ's 8878.
//!
//! Lab: needs the lab media (the proxy and the 1440p MKV), VideoToolbox, and ffmpeg (a dev
//! tool, for the comparison only). Writes its numbers to `$WALK_OUT` (JSON) when set.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

mod walkcommon;
use walkcommon::FF;

/// The reference reel's folder in the lab media folder.
fn reel_dir() -> &'static Path {
    static P: std::sync::LazyLock<std::path::PathBuf> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media("h79"));
    P.as_path()
}
const F: i64 = 705_600_000;

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: lab media, VideoToolbox, ffmpeg"]
fn reproduce_h79_v4() {
    if !reel_dir().join("render-spec-v4.json").exists() || !Path::new(FF).exists() {
        eprintln!("skipped: no lab media or ffmpeg");
        return;
    }
    let mut report = serde_json::Map::new();
    let mut steps: Vec<Value> = Vec::new();
    let t0 = Instant::now();
    let mut step = |what: &str, ok: bool, note: &str| {
        eprintln!(
            "[{:>6.1}s] {} {what} {note}",
            t0.elapsed().as_secs_f64(),
            if ok { "ok " } else { "GAP" }
        );
        steps.push(json!({"t_s": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0, "step": what, "ok": ok, "note": note}));
    };
    let root = std::env::temp_dir().join(format!("gmclip-walk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });

    // 1. Home › open loc_4fadb756 (moments with hook ids, transcript, the proxy + 1440p
    //    MKV group with its offsets)
    let imp = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    let pid = imp["id"].as_str().unwrap().to_owned();
    step(
        "open the loc_4fadb756 project",
        true,
        &format!("{} moments", imp["fixture"]["moments"]),
    );

    // 2. Moments › find h79 (imported; no agent run) and stage it
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let m = doc
        .iter::<gmclip_model::entities::Moment>()
        .find(|m| m.origin_ref.as_deref() == Some("loc_4fadb756_h79"))
        .expect("h79 moment")
        .clone();
    c.call(
        &pid,
        "moment_set_state",
        json!({"moment": m.id.0.to_string(), "state": "staged"}),
    )
    .unwrap();
    step("stage the h79 moment", true, "review mode s");

    // 3. Make a cut from the moment (the new button / g)
    let made = gmclip_app::cutnew::from_moment(&c, &pid, &m.id.0.to_string()).unwrap();
    let cut = made["cut"].as_str().unwrap().to_owned();
    step(
        "make a cut from the moment",
        true,
        &format!("{} s seed", made["seconds"]),
    );

    // 4. Story: RT's 14 takes, typed in/out (the story panel's fields), in RT's order
    let spec: Value =
        serde_json::from_slice(&std::fs::read(reel_dir().join("render-spec-v4.json")).unwrap())
            .unwrap();
    let segs = spec["segments"].as_array().unwrap().clone();
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(&cut).unwrap();
    let first = gmclip_model::clock::cut_layout(&doc, cid)[0].part;
    let media = doc
        .get::<gmclip_model::entities::Part>(first)
        .unwrap()
        .media;
    let mut after: Option<String> = None;
    let mut part_ids = Vec::new();
    for s in &segs {
        let a = (s["native_start"].as_f64().unwrap() * 30.0).round() as i64 * (F / 30);
        let b = (s["native_end"].as_f64().unwrap() * 30.0).round() as i64 * (F / 30);
        let mut args = json!({"cut": cut, "media": media, "src_in": a, "src_out": b});
        if let Some(x) = &after {
            args["after"] = json!(x);
        }
        let r = c.call(&pid, "part_add", args).unwrap();
        let id = r["output"]["id"].as_str().unwrap().to_owned();
        after = Some(id.clone());
        part_ids.push(id);
    }
    c.call(
        &pid,
        "cut_edit",
        json!({"cut": cut, "ops": [{"op": "part_delete", "part": first.0.to_string()}]}),
    )
    .unwrap();
    let pl = c.planned(&pid, &cut).unwrap().plan;
    step(
        "14 takes typed in (story panel)",
        pl.parts.len() == 14,
        &format!(
            "{} frames ({:.2} s); RT 1807",
            pl.output.frames,
            pl.output.frames as f64 / 30.0
        ),
    );
    report.insert("frames".into(), json!(pl.output.frames));

    // 5. Layout: screen + facecam per take, RT's crops and places (crop editor / inspector)
    // By numbers, the way AJ would now: the facecam once on take 2 and "apply to all";
    // then per distinct layout its screen crop/place (and insets) typed on its first take,
    // "copy to the next N" over a run of takes that share it, and a saved preset for a
    // layout that comes back later. Every UI action is counted.
    let nums = |v: &Value| -> Vec<f64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect()
    };
    let mut layout_errs: Vec<String> = Vec::new();
    let (mut typed, mut clicks) = (0usize, 0usize);
    let ids = |k: usize, role: &str| -> (String, String) {
        let cv = c.cut_view(&pid, &cut).unwrap();
        let p = &cv["parts"][k];
        let l = p["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["role"] == role)
            .cloned()
            .unwrap_or(Value::Null);
        (
            p["plan_id"].as_str().unwrap_or_default().to_owned(),
            l["id"].as_str().unwrap_or_default().to_owned(),
        )
    };
    let type_layer = |k: usize, role: &str, crop: &Value, place: &Value| -> Result<(), String> {
        let (pp, pl) = ids(k, role);
        let (cr, pc) = (nums(crop), nums(place));
        c.crop_px(&pid, &cut, &pp, &pl, [cr[0], cr[1], cr[2], cr[3]], None)
            .map_err(|e| e.message)?;
        let (pp, pl) = ids(k, role);
        c.place_px(&pid, &cut, &pp, &pl, pc[0], pc[1], pc[2], None)
            .map_err(|e| e.message)?;
        Ok(())
    };
    // the facecam: typed once (7 numbers), then "apply to all parts"
    let cam_seg = segs
        .iter()
        .position(|s| s["camera_crop"][2].as_f64() != Some(32.0))
        .unwrap();
    if let Err(e) = type_layer(
        cam_seg,
        "cam",
        &segs[cam_seg]["camera_crop"],
        &segs[cam_seg]["camera_place"],
    ) {
        layout_errs.push(format!("cam: {e}"));
    }
    typed += 7;
    gmclip_app::framing::apply_to_all(&c, &pid, &cut, &part_ids[cam_seg]).unwrap();
    clicks += 1;
    // a take's look, as RT gives it (the camera is hidden separately on 3 takes)
    let sig = |s: &Value| -> String {
        format!(
            "{}{}{}",
            s["screen_crop"], s["screen_place"], s["detail_layers"]
        )
    };
    let mut saved: std::collections::BTreeMap<String, String> = Default::default();
    let mut k = 0;
    while k < segs.len() {
        let s = &segs[k];
        let me = sig(s);
        if let Some(name) = saved.get(&me) {
            // pick the preset, "use on this part"
            if let Err(e) = gmclip_app::takelayout::preset_apply(
                &c,
                &pid,
                &cut,
                name,
                std::slice::from_ref(&part_ids[k]),
            ) {
                layout_errs.push(format!("{}: preset: {}", s["id"], e.message));
            }
            clicks += 2;
        } else {
            if let Err(e) = type_layer(k, "screen", &s["screen_crop"], &s["screen_place"]) {
                layout_errs.push(format!("{}: screen: {e}", s["id"]));
            }
            typed += 7;
            for d in s["detail_layers"].as_array().unwrap() {
                let (cr, pc) = (nums(&d["crop"]), nums(&d["place"]));
                if let Err(e) = gmclip_app::takelayout::detail_add(
                    &c,
                    &pid,
                    &cut,
                    &part_ids[k],
                    Some([cr[0], cr[1], cr[2], cr[3]]),
                    Some([pc[0], pc[1], pc[2]]),
                ) {
                    layout_errs.push(format!("{}: inset: {}", s["id"], e.message));
                }
                clicks += 1;
                typed += 7;
            }
            // a layout that comes back after another one: save it (name + click)
            let run_end = (k + 1..segs.len())
                .find(|&j| sig(&segs[j]) != me)
                .unwrap_or(segs.len());
            if segs[run_end..].iter().any(|x| sig(x) == me) {
                let name = s["id"].as_str().unwrap().to_owned();
                gmclip_app::takelayout::preset_save(&c, &pid, &cut, &part_ids[k], &name).unwrap();
                saved.insert(me.clone(), name);
                typed += 1;
                clicks += 1;
            }
        }
        // the takes right after that share it: "copy to the next N"
        let run = (k + 1..segs.len())
            .take_while(|&j| sig(&segs[j]) == me)
            .count();
        if run > 0 {
            if let Err(e) =
                gmclip_app::takelayout::copy_layout(&c, &pid, &cut, &part_ids[k], Some(run))
            {
                layout_errs.push(format!("{}: copy: {}", s["id"], e.message));
            }
            typed += 1;
            clicks += 1;
        }
        k += 1 + run;
    }
    // every take now has RT's screen crop and place and its insets (within a pixel)
    let cv = c.cut_view(&pid, &cut).unwrap();
    let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1.0);
    let mut matched = 0;
    for (k, s) in segs.iter().enumerate() {
        let ls = cv["parts"][k]["layers"].as_array().unwrap();
        let screen = ls.iter().find(|l| l["role"] == "screen");
        let dets: Vec<&Value> = ls.iter().filter(|l| l["role"] == "detail").collect();
        let want = s["detail_layers"].as_array().unwrap();
        let ok = screen.is_some_and(|l| {
            close(&nums(&l["crop"]), &nums(&s["screen_crop"]))
                && close(&nums(&l["place"])[..3], &nums(&s["screen_place"])[..3])
        }) && dets.len() == want.len()
            && dets.iter().zip(want).all(|(l, d)| {
                close(&nums(&l["crop"]), &nums(&d["crop"]))
                    && close(&nums(&l["place"])[..3], &nums(&d["place"])[..3])
            });
        if ok {
            matched += 1;
        } else {
            layout_errs.push(format!("{}: differs from RT", s["id"]));
        }
    }
    report.insert(
        "layout_actions".into(),
        json!({"typed_values": typed, "clicks": clicks, "takes_matching_rt": matched}),
    );
    step(
        "per-take layout by numbers (presets, copy to next N, insets)",
        layout_errs.is_empty(),
        &format!(
            "{matched}/14 takes match RT; {typed} typed values + {clicks} clicks, no drags; {}",
            layout_errs.join("; ")
        ),
    );
    // camera hidden on the takes where RT used a dummy camera crop
    let cv = c.cut_view(&pid, &cut).unwrap();
    let mut hides = 0;
    for (k, s) in segs.iter().enumerate() {
        if s["camera_crop"][2].as_f64() == Some(32.0) {
            let out = &cv["parts"][k]["out"];
            if gmclip_app::sound::hide_cam(
                &c,
                &pid,
                &cut,
                out[0].as_i64().unwrap(),
                out[1].as_i64().unwrap(),
            )
            .is_ok()
            {
                hides += 1;
            }
        }
    }
    step(
        "hide the camera on 3 full-screen takes",
        hides == 3,
        &format!("{hides}/3"),
    );

    // 6. The Short: hook, pill, measured run clock, captions, gain on the generated voice
    let hk = gmclip_app::short::hook_set(
        &c,
        &pid,
        &cut,
        "My PC made this in *67 seconds.*",
        None,
        Some(5870),
    );
    step(
        "text hook from frame 0 (5.87 s)",
        hk.is_ok(),
        &hk.err().map(|e| e.message).unwrap_or_default(),
    );
    let pill = gmclip_app::short::pill_add_span(
        &c,
        &pid,
        &cut,
        &part_ids[1],
        Some(&part_ids[5]),
        "MiniMax H3 *running on my PC*",
    );
    step(
        "section pill over takes 2–6",
        pill.is_ok(),
        &pill.err().map(|e| e.message).unwrap_or_default(),
    );
    let cv = c.cut_view(&pid, &cut).unwrap();
    let start_f = cv["parts"][6]["out"][0].as_u64().unwrap();
    let end_f = cv["parts"][8]["out"][0].as_u64().unwrap();
    let marked = gmclip_app::short::run_measure(&c, &pid, &cut, start_f, end_f);
    step(
        "measure the run by marking two frames",
        marked.is_ok(),
        &format!(
            "{:?}: only frames the cut shows can be marked",
            marked
                .as_ref()
                .map(|v| v["seconds"].clone())
                .map_err(|e| e.message.clone())
        ),
    );
    // the run the cut does not show: from the transcript word before each event, browse
    // the stream there and step to the frame (RT's queue 0→1 at 2610.833, the result at
    // 2678.267), marking each
    let tv = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    let starts: Vec<f64> = tv["start_ms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap_or(0.0) / 1000.0)
        .collect();
    let mut steps = Vec::new();
    let mut marks = Vec::new();
    for want in [2610.833, 2678.267] {
        let word = starts
            .iter()
            .copied()
            .filter(|s| *s <= want)
            .fold(0.0, f64::max);
        let (_, info) = gmclip_app::short::source_view(&c, &pid, &cut, word).unwrap();
        let (t0, fps) = (
            info["t0_s"].as_f64().unwrap(),
            info["fps"].as_f64().unwrap(),
        );
        let target = ((want - t0) * fps).round();
        steps.push((target - info["frame"].as_f64().unwrap()).abs() as i64);
        marks.push(t0 + target / fps);
    }
    let m = gmclip_app::short::run_measure_src(&c, &pid, &cut, marks[0], marks[1]);
    let secs = m.as_ref().ok().and_then(|v| v["seconds"].as_f64());
    report.insert(
        "run_measure".into(),
        json!({"marks_s": marks, "frame_steps": steps, "seconds": secs}),
    );
    step(
        "measure the run in the stream viewer (outside the cut)",
        m.is_ok() && secs.is_some_and(|s| (s - 67.4).abs() < 0.1),
        &format!(
            "{secs:?} s, RT 67.4 s; marks {:.3} → {:.3} s, {} + {} frame steps from the transcript word",
            marks[0], marks[1], steps[0], steps[1]
        ),
    );
    if let Ok(mv) = &m {
        // as RT: from take 7 (where the run starts) until take 10 ends
        let ck = gmclip_app::short::clock_add_span(
            &c,
            &pid,
            &cut,
            mv["measurement"].as_str().unwrap(),
            "GENERATING",
            "DONE IN",
            Some(&part_ids[6]),
            Some(&part_ids[9]),
        );
        step(
            "run clock pill",
            ck.is_ok(),
            &ck.err().map(|e| e.message).unwrap_or_default(),
        );
    }
    let g1 = gmclip_app::sound::gain_span(&c, &pid, &cut, 1, 54, 10.0);
    let g2 = gmclip_app::sound::gain_span(&c, &pid, &cut, 1332, 1399, 10.0);
    step(
        "+10 dB on the generated voice (2 spans)",
        g1.is_ok() && g2.is_ok(),
        "50 ms ramps (RT 80/120 ms)",
    );
    // no captions over the two full-screen banana takes (the clip's own text speaks)
    let cv = c.cut_view(&pid, &cut).unwrap();
    let mut quiet = 0;
    for k in [0usize, 10] {
        let out = &cv["parts"][k]["out"];
        if gmclip_app::sound::no_captions(
            &c,
            &pid,
            &cut,
            out[0].as_i64().unwrap(),
            out[1].as_i64().unwrap(),
        )
        .is_ok()
        {
            quiet += 1;
        }
    }
    step(
        "no captions on takes 1 and 11",
        quiet == 2,
        &format!("{quiet}/2"),
    );
    // RT's caption keywords: one comma list, starred across the cut (RT's `keywords`)
    let set = gmclip_app::short::keywords_set(
        &c,
        &pid,
        &cut,
        "two, minutes, voice, run, NVIDIA, 5080, GPU, locally, local",
    );
    let marked = c
        .planned(&pid, &cut)
        .map(|p| {
            p.plan
                .text_events
                .iter()
                .flatten()
                .filter(|t| t.role == gmclip_plan::types::TextRole::Caption)
                .flat_map(|t| t.runs.iter())
                .filter(|r| r.fill.is_some())
                .count()
        })
        .unwrap_or(0);
    let marked = if set.is_ok() { marked } else { 0 };
    step(
        "caption keywords (★) like RT's",
        marked > 0,
        &format!("{marked} starred caption words from one keywords list"),
    );
    let pl = match c.planned(&pid, &cut) {
        Ok(p) => p.plan,
        Err(e) => {
            step("the cut plans", false, &e.message);
            panic!("{}", e.message);
        }
    };
    let caps: Vec<&gmclip_plan::types::TextEvent> = pl
        .text_events
        .iter()
        .flatten()
        .filter(|t| t.role == gmclip_plan::types::TextRole::Caption)
        .collect();
    step(
        "captions from the cut's own words",
        !caps.is_empty(),
        &format!("{} cues", caps.len()),
    );

    // 7. Export (9:16 → −16 LUFS static by default)
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let te = Instant::now();
    let id = jobs.start_export(&c, &pid, &cut).unwrap()["job"]
        .as_str()
        .unwrap()
        .to_owned();
    let ok = loop {
        let s = jobs.list().into_iter().find(|x| x.id == id).unwrap();
        if s.state == "done" {
            break Ok(());
        }
        if s.state == "failed" {
            break Err(s.error.unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    step(
        "export",
        ok.is_ok(),
        &format!(
            "{:.1} s; {}",
            te.elapsed().as_secs_f64(),
            ok.clone().err().unwrap_or_default()
        ),
    );
    let ex = c.exports(&pid).unwrap();
    let ours = ex[0].file.to_string_lossy().into_owned();
    let theirs = reel_dir().join("h79-v4.mp4").to_string_lossy().into_owned();

    // 8. Compare with RT's MP4
    report.insert("export_frames".into(), json!(ex[0].frames));
    let note = walkcommon::compare(
        &pl,
        &ours,
        &theirs,
        &reel_dir().join("captions-v4.ass"),
        &root,
        "h79",
        &["1.0", "10.0", "27.0", "45.0", "57.0"],
        &mut report,
    );
    step("compare with RT", true, &note);

    // 9. Send to Clippy, to a local stand-in (never AJ's 8878)
    gmclip_app::sheet::install(Box::new(|_, _, _| true));
    let paired = gmclip_app::board::pair(&c).is_ok();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let got = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut buf = vec![0u8; 1 << 16];
        let n = s.read(&mut buf).unwrap_or(0);
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
            .ok();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    });
    let sent = gmclip_app::board::send_to(&c, &pid, &ex[0].id, &format!("http://127.0.0.1:{port}"));
    let req = got.join().unwrap_or_default();
    step(
        "Send to Clippy (local stand-in)",
        paired && sent.is_ok() && req.contains("loc_4fadb756_h79"),
        &sent.err().map(|e| e.message).unwrap_or_default(),
    );

    report.insert("steps".into(), json!(steps));
    report.insert("export".into(), json!(ours));
    let out = serde_json::to_string_pretty(&Value::Object(report)).unwrap();
    eprintln!("{out}");
    if let Some(p) = std::env::var_os("WALK_OUT") {
        std::fs::write(p, &out).unwrap();
    }
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
}
