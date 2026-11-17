//! E.01 / U.16: a batch runs one cut at a time, skips a cut the human edited after it
//! started ("skipped: you edited it"), and when its wall clock runs out mid-run it stops
//! that run and reports the cuts after it as not run. The runs are a stand-in here (the
//! launcher path is covered by findmoments.spec, batchcreds.spec and the holder's
//! launched-credential tests); the wall clock is shortened through `start_with` instead
//! of waiting hours.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}
const F: i64 = 705_600_000;

fn wait_done(c: &AppCore, pid: &str, id: &str) -> Value {
    for _ in 0..400 {
        let l = gmclip_app::batch::list(c, pid);
        let b = l["batches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == id)
            .cloned()
            .unwrap();
        if b["state"] == "done" || b["state"] == "stopped" {
            return b;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the batch did not finish");
}

/// The app with h79 open and `n` cuts (the video's own, then `n - 1` new 3 s ones).
fn setup(tag: &str, n: usize) -> Option<(Arc<AppCore>, String, Vec<String>, std::path::PathBuf)> {
    if !Path::new(video()).is_file() {
        return None;
    }
    let root = std::env::temp_dir().join(format!("gmclip-batch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, a) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(&a).unwrap();
    let media = doc
        .get::<gmclip_model::entities::Part>(gmclip_model::clock::cut_layout(&doc, cid)[0].part)
        .unwrap()
        .media;
    let mut cuts = vec![a.clone()];
    for t in ["b", "c", "d", "e"].into_iter().take(n - 1) {
        let id = c.call(&pid, "cut_create", json!({"title": t})).unwrap()["output"]["cut"]
            .as_str()
            .unwrap()
            .to_owned();
        c.call(
            &pid,
            "part_add",
            json!({"cut": id, "media": media, "src_in": 0, "src_out": 3 * F}),
        )
        .unwrap();
        cuts.push(id);
    }
    Some((c, pid, cuts, root))
}

#[test]
fn one_at_a_time_skipping_edited_cuts_and_the_wall_clock() {
    let Some((c, pid, cuts, root)) = setup("edit", 3) else {
        return;
    };
    // the stand-in: while cut a runs, the human edits cut c; it records the order
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (o2, c2, pid2, cut_c) = (order.clone(), c.clone(), pid.clone(), cuts[2].clone());
    let runner: Arc<gmclip_app::batch::RunOne> = Arc::new(move |cut: &str, _deadline| {
        o2.lock().unwrap().push(cut.to_owned());
        if o2.lock().unwrap().len() == 1 {
            c2.call(
                &pid2,
                "cut_set",
                json!({"cut": cut_c, "title": "c (edited)"}),
            )
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
        gmclip_app::batch::CutEnd::Done(format!("run-for-{cut}"))
    });
    let b = gmclip_app::batch::start_with(
        &c,
        &pid,
        &cuts,
        "edit_pass",
        Duration::from_secs(60),
        runner.clone(),
    )
    .unwrap();
    assert_eq!(b["budget_usd"], 0.0);
    // §10.7.1: the pre-approval for AI passes within the budget is recorded at launch
    assert_eq!(b["preapproval"]["covers"], json!(["ai_pass"]));
    assert_eq!(b["preapproval"]["by"], "human");
    assert_eq!(b["per_cut_s"], 60);
    let done = wait_done(&c, &pid, b["id"].as_str().unwrap());
    let states: Vec<&str> = done["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["done", "done", "skipped"], "{done}");
    assert_eq!(done["cuts"][2]["reason"], "skipped: you edited it");
    assert_eq!(
        *order.lock().unwrap(),
        vec![cuts[0].clone(), cuts[1].clone()]
    );
    assert_eq!(done["state"], "done");
    // a wall clock already spent: no cut runs, each reported as not run
    let b =
        gmclip_app::batch::start_with(&c, &pid, &cuts[..2], "edit_pass", Duration::ZERO, runner)
            .unwrap();
    let done = wait_done(&c, &pid, b["id"].as_str().unwrap());
    assert_eq!(done["state"], "stopped", "{done}");
    assert!(
        done["cuts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["state"] == "not_run" && x["reason"] == gmclip_app::batch::NOT_RUN),
        "{done}"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The wall clock runs out while the second of four cuts runs: that run is told the
/// batch's deadline and stops there (as the launcher runner does: its credential ended),
/// the batch reports it `stopped` and the two cuts after it `not_run`, and the batch ends
/// within moments of its deadline instead of running on.
#[test]
fn the_wall_clock_running_out_mid_run_stops_the_batch_cleanly() {
    let Some((c, pid, cuts, root)) = setup("clock", 4) else {
        return;
    };
    let started = Arc::new(Mutex::new(Vec::<String>::new()));
    let s2 = started.clone();
    let runner: Arc<gmclip_app::batch::RunOne> = Arc::new(move |cut: &str, deadline: Instant| {
        let n = {
            let mut g = s2.lock().unwrap();
            g.push(cut.to_owned());
            g.len()
        };
        if n == 1 {
            std::thread::sleep(Duration::from_millis(100));
            return gmclip_app::batch::CutEnd::Done(format!("run-{n}"));
        }
        // a long run: it would go on for minutes, but it is stopped at the deadline
        let until = Instant::now() + Duration::from_secs(120);
        while Instant::now() < until {
            if Instant::now() >= deadline {
                return gmclip_app::batch::CutEnd::Stopped(Some(format!("run-{n}")));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        gmclip_app::batch::CutEnd::Done(format!("run-{n}"))
    });
    let wall = Duration::from_millis(800);
    let t0 = Instant::now();
    let b = gmclip_app::batch::start_with(&c, &pid, &cuts, "edit_pass", wall, runner).unwrap();
    assert_eq!(b["wall_clock_s"], 0, "the shortened clock (under a second)");
    let done = wait_done(&c, &pid, b["id"].as_str().unwrap());
    let took = t0.elapsed();
    eprintln!("batch over 4 cuts, {wall:?} wall clock: ended after {took:?}: {done}");
    assert!(took >= wall, "it ran until its wall clock ({took:?})");
    assert!(
        took < wall + Duration::from_millis(1500),
        "it stopped at its wall clock, not after the run ({took:?})"
    );
    assert_eq!(done["state"], "stopped", "{done}");
    let states: Vec<&str> = done["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["done", "stopped", "not_run", "not_run"], "{done}");
    assert_eq!(done["cuts"][1]["run"], "run-2", "the stopped run is named");
    assert_eq!(done["cuts"][1]["reason"], gmclip_app::batch::STOPPED);
    for k in [2, 3] {
        assert_eq!(done["cuts"][k]["reason"], gmclip_app::batch::NOT_RUN);
        assert!(done["cuts"][k]["run"].is_null(), "never started");
    }
    assert_eq!(
        *started.lock().unwrap(),
        cuts[..2].to_vec(),
        "no run started after the wall clock ran out"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
