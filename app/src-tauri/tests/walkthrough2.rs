//! The second M2 walkthrough (coordinator 2026-10-15): AJ's h03 reel on another stream
//! ("My Discord now has its own website", loc_086da141, 39.6 s, story v5), chosen for
//! what h79 did not exercise: takes out of stream order, `hold_at` screen holds (one on a
//! frame after its take, one before), four section pills in turn, the opening's own
//! `*_hook` layout, project keep pairs, multi-word caption substitutions. Same method as
//! the first: the app functions the UI's buttons call, no model runs, Send to Clippy not
//! needed here. Lab: the lab media (the reel, the proxy), VideoToolbox, ffmpeg (comparison only).
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
        std::sync::LazyLock::new(|| gmclip_model::lab::media("reels/loc_086da141_h03"));
    P.as_path()
}
const F: i64 = 705_600_000;

fn nums(v: &Value) -> Vec<f64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: lab media, VideoToolbox, ffmpeg"]
fn reproduce_h03_v5() {
    if !reel_dir().join("plan.json").exists() || !Path::new(FF).exists() {
        eprintln!("skipped: no reference reel or ffmpeg");
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
    let root = std::env::temp_dir().join(format!("gmclip-walk2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let plan: Value =
        serde_json::from_slice(&std::fs::read(reel_dir().join("plan.json")).unwrap()).unwrap();
    let spec: Value =
        serde_json::from_slice(&std::fs::read(reel_dir().join("render-spec-v5.json")).unwrap())
            .unwrap();
    let segs = spec["segments"].as_array().unwrap().clone();
    let cuts = plan["cuts"].as_array().unwrap().clone();

    // 1. import the stream, 2. the moment, 3. a cut from it
    let imp = gmclip_app::labfixture::open_project(&c, "loc_086da141").unwrap();
    let pid = imp["id"].as_str().unwrap().to_owned();
    step(
        "open the loc_086da141 project",
        true,
        &format!("{} moments", imp["fixture"]["moments"]),
    );
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let m = doc
        .iter::<gmclip_model::entities::Moment>()
        .find(|m| m.origin_ref.as_deref() == Some("loc_086da141_h03"))
        .expect("h03 moment")
        .clone();
    let made = gmclip_app::cutnew::from_moment(&c, &pid, &m.id.0.to_string()).unwrap();
    let cut = made["cut"].as_str().unwrap().to_owned();
    step(
        "make a cut from the moment",
        true,
        &format!("{} s seed", made["seconds"]),
    );

    // 4. RT's 11 takes, typed in/out, in RT's (non-chronological) order
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
    for k in &cuts {
        let a = (k["a"].as_f64().unwrap() * 30.0).round() as i64 * (F / 30);
        let b = (k["z"].as_f64().unwrap() * 30.0).round() as i64 * (F / 30);
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
    let rt_frames = (39.566667_f64 * 30.0).round() as u64;
    step(
        "11 takes typed in, out of stream order",
        pl.parts.len() == 11,
        &format!("{} frames; RT {rt_frames}", pl.output.frames),
    );
    report.insert("frames".into(), json!(pl.output.frames));

    // 5. layout by numbers: take 2's facecam and screen ("win"), apply to all; take 1's
    //    own screen (the opening's chat_hook layout)
    let (mut typed, mut clicks) = (0usize, 0usize);
    let mut errs: Vec<String> = Vec::new();
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
        let (cr, pc) = (nums(crop), nums(place));
        let (pp, pl) = ids(k, role);
        c.crop_px(&pid, &cut, &pp, &pl, [cr[0], cr[1], cr[2], cr[3]], None)
            .map_err(|e| e.message)?;
        let (pp, pl) = ids(k, role);
        c.place_px(&pid, &cut, &pp, &pl, pc[0], pc[1], pc[2], None)
            .map_err(|e| e.message)?;
        Ok(())
    };
    for (role, crop, place) in [
        ("cam", "camera_crop", "camera_place"),
        ("screen", "screen_crop", "screen_place"),
    ] {
        if let Err(e) = type_layer(1, role, &segs[1][crop], &segs[1][place]) {
            errs.push(format!("take 2 {role}: {e}"));
        }
        typed += 7;
    }
    gmclip_app::framing::apply_to_all(&c, &pid, &cut, &part_ids[1]).unwrap();
    clicks += 1;
    if let Err(e) = type_layer(
        0,
        "screen",
        &segs[0]["screen_crop"],
        &segs[0]["screen_place"],
    ) {
        errs.push(format!("take 1 screen: {e}"));
    }
    typed += 7;
    let cv = c.cut_view(&pid, &cut).unwrap();
    let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1.0);
    let mut matched = 0;
    for (k, s) in segs.iter().enumerate() {
        let ls = cv["parts"][k]["layers"].as_array().unwrap();
        let get = |r: &str| ls.iter().find(|l| l["role"] == r).cloned();
        let ok = ["screen", "cam"].iter().all(|r| {
            let (cr, pc) = if *r == "screen" {
                ("screen_crop", "screen_place")
            } else {
                ("camera_crop", "camera_place")
            };
            get(r).is_some_and(|l| {
                close(&nums(&l["crop"]), &nums(&s[cr]))
                    && close(&nums(&l["place"])[..3], &nums(&s[pc])[..3])
            })
        });
        if ok {
            matched += 1;
        } else {
            errs.push(format!("{}: differs from RT", s["id"]));
        }
    }
    step(
        "layout by numbers (two layouts, apply to all)",
        errs.is_empty(),
        &format!(
            "{matched}/11 takes match RT; {typed} typed values + {clicks} click; {}",
            errs.join("; ")
        ),
    );

    // 6. the two screen holds (RT's hold_at): typed stream time + Hold, in the inspector
    let mut holds = 0;
    for (k, kk) in cuts.iter().enumerate() {
        if let Some(t) = kk["hold_at"].as_f64() {
            match gmclip_app::tighten::hold_part_at(&c, &pid, &cut, &part_ids[k], t) {
                Ok(_) => holds += 1,
                Err(e) => errs.push(format!("{}: hold: {}", kk["id"], e.message)),
            }
        }
    }
    let pl = c.planned(&pid, &cut).unwrap().plan;
    let held: Vec<Value> = pl
        .parts
        .iter()
        .flat_map(|p| p.layers.iter().map(move |l| (p, l)))
        .filter_map(|(p, l)| {
            l.time_map
                .as_ref()
                .map(|tm| json!({"part": p.id.as_str(), "layer": l.id.as_str(), "time_map": tm}))
                .filter(|v| v["time_map"]["mode"] == "hold")
        })
        .collect();
    report.insert("holds".into(), json!(held));
    step(
        "screen held on stream frames 122.1 s and 126.05 s (outside their takes)",
        holds == 2 && held.len() == 2,
        &format!("{holds}/2; {} held layers in the plan", held.len()),
    );

    // 7. the Short: hook over take 1, four pills in turn, keywords, keep pairs, fixes
    let hk = gmclip_app::short::hook_set(
        &c,
        &pid,
        &cut,
        "My Discord now has its own *website.*",
        None,
        Some(4933),
    );
    step(
        "text hook over the opening take",
        hk.is_ok(),
        &hk.err().map(|e| e.message).unwrap_or_default(),
    );
    let pill_text = |k: &str| {
        plan["pills"][k]["text"]
            .as_str()
            .unwrap()
            .replace("{Y}", "*")
            .trim_end()
            .to_owned()
    };
    let mut pills = 0;
    let mut k = 0;
    while k < cuts.len() {
        let Some(name) = cuts[k]["pill"].as_str() else {
            k += 1;
            continue;
        };
        let mut j = k;
        while j + 1 < cuts.len() && cuts[j + 1]["pill"].as_str() == Some(name) {
            j += 1;
        }
        let mut text = pill_text(name);
        if text.matches('*').count() == 1 {
            text.push('*');
        }
        match gmclip_app::short::pill_add_span(
            &c,
            &pid,
            &cut,
            &part_ids[k],
            Some(&part_ids[j]),
            &text,
        ) {
            Ok(_) => pills += 1,
            Err(e) => errs.push(format!("pill {name}: {}", e.message)),
        }
        k = j + 1;
    }
    step(
        "four section pills in turn",
        pills == 4,
        &format!("{pills}/4 (one per run of takes)"),
    );
    gmclip_app::short::captions_set(&c, &pid, &cut, true).ok();
    // RT's caption_y: 1110 under this reel's taller panel (one typed value)
    let cy = gmclip_app::short::caption_y_set(&c, &pid, &cut, 1110.0);
    let seat = c.planned(&pid, &cut).ok().and_then(|p| {
        p.plan
            .text_events
            .as_ref()?
            .iter()
            .find(|e| e.role == gmclip_plan::types::TextRole::Caption)
            .map(|e| e.pos.y)
    });
    step(
        "caption height 1110 (RT's caption_y)",
        cy.is_ok() && seat.is_some_and(|y| (y - 1110.0).abs() < 1.0),
        &format!("{seat:?}"),
    );
    let kw: Vec<&str> = plan["keywords"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let kw_ok = gmclip_app::short::keywords_set(&c, &pid, &cut, &kw.join(", ")).is_ok();
    let mut keeps = 0;
    for pair in plan["keep"].as_array().unwrap() {
        let w: Vec<&str> = pair
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if c.call(&pid, "caption_keep_set", json!({"words": w}))
            .is_ok()
        {
            keeps += 1;
        }
    }
    step(
        "keywords list and keep pairs",
        kw_ok && keeps == 10,
        &format!("{} keywords, {keeps}/10 keep pairs", kw.len()),
    );
    // RT's corrections, found in our transcript and replaced in the captions (⌘F replace)
    let tv = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    let rev = tv["revision"].as_str().unwrap_or_default().to_owned();
    let text: Vec<String> = tv["text"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| norm(t.as_str().unwrap_or_default()))
        .collect();
    let frame: Vec<i64> = tv["frame"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap_or(-1))
        .collect();
    let mut subs: Vec<(String, String)> = plan["cap_sub"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p[0].as_str().unwrap().to_owned(),
                p[1].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    for (k, v) in plan["fix"].as_object().unwrap() {
        subs.push((k.clone(), v.as_str().unwrap_or_default().to_owned()));
    }
    let mut replaced = Vec::new();
    for (from, to) in &subs {
        if to.is_empty() {
            continue; // "um" → "": fillers are dropped from captions already
        }
        let q: Vec<String> = from.split_whitespace().map(norm).collect();
        let hits: Vec<Vec<u64>> = (0..text.len().saturating_sub(q.len() - 1))
            .filter(|&i| frame[i] >= 0 && q.iter().enumerate().all(|(j, w)| text[i + j] == *w))
            .map(|i| (i..i + q.len()).map(|x| x as u64).collect())
            .collect();
        if !hits.is_empty()
            && gmclip_app::short::caption_replace(&c, &pid, &cut, &rev, &hits, to).is_ok()
        {
            replaced.push(format!("{from} → {to} ×{}", hits.len()));
        }
    }
    step(
        "RT's caption corrections where our transcript has them",
        true,
        &format!(
            "{} of {} applied: {}",
            replaced.len(),
            subs.len(),
            replaced.join("; ")
        ),
    );
    let pl = match c.planned(&pid, &cut) {
        Ok(p) => p.plan,
        Err(e) => {
            step("the cut plans", false, &e.message);
            panic!("{}", e.message);
        }
    };

    // 8. export and compare
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
    let theirs = reel_dir()
        .join("loc_086da141_h03-v5.mp4")
        .to_string_lossy()
        .into_owned();
    report.insert("export_frames".into(), json!(ex[0].frames));
    let note = walkcommon::compare(
        &pl,
        &ours,
        &theirs,
        &reel_dir().join("captions-v5.ass"),
        &root,
        "h03",
        &["1.0", "8.0", "16.0", "27.0", "36.5"],
        &mut report,
    );
    step("compare with RT", true, &note);
    report.insert("steps".into(), json!(steps));
    if let Some(out) = std::env::var_os("WALK_OUT") {
        std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
