//! D.02: the timeline's `gm://data` columns — `peaks`, `energy`, `quiet` and `words` — on
//! the cut's output clock, each after the 16-byte header. On AJ's h79 v4 reel (lab): the
//! header, a level per output frame, quiet regions inside the cut, and the words column
//! equal to the words strip's played words; then cut down to two parts out of order, the
//! columns follow the edit. `three_hour_columns` (ignored: decodes two hours of audio)
//! times building them for a 3 h cut and is the row in `milestones/m2/perf.md`.

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}
/// (a): the Oct-01 restream, 2 h 1440p H.264 + AAC (test-media/manifest.toml).
/// `streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4` in the lab media folder.
fn long() -> &'static str {
    static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        gmclip_model::lab::media_str(
            "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
        )
    });
    P.as_str()
}

const FLICKS: i64 = 705_600_000;

fn core_at(root: &Path) -> std::sync::Arc<AppCore> {
    AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    })
}

struct Col {
    version: u16,
    kind: u16,
    count: u32,
    rev: u64,
    body: Vec<u8>,
}

fn get(c: &AppCore, pid: &str, cut: &str, kind: &str) -> Col {
    let b = gmclip_app::ipc::gm_data(c, &format!("gm://data/{pid}/{kind}?cut={cut}"))
        .unwrap_or_else(|| panic!("{kind}"));
    assert!(b.len() >= 16);
    Col {
        version: u16::from_le_bytes([b[0], b[1]]),
        kind: u16::from_le_bytes([b[2], b[3]]),
        count: u32::from_le_bytes(b[4..8].try_into().unwrap()),
        rev: u64::from_le_bytes(b[8..16].try_into().unwrap()),
        body: b[16..].to_vec(),
    }
}

fn f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// `(ordinal, output frame)` of every word the words column lists, and its text.
fn word_rows(c: &Col) -> Vec<(u32, u32, String)> {
    let n = c.count as usize;
    let table = u32s(&c.body[..n * 28]);
    let text = &c.body[n * 28..];
    table
        .chunks_exact(7)
        .map(|r| {
            let t = &text[r[5] as usize..(r[5] + r[6]) as usize];
            (r[0], r[3], String::from_utf8(t.to_vec()).unwrap())
        })
        .collect()
}

/// How long the analysis may go without moving (its phase or the seconds heard) before
/// the wait calls it hung. Not a bound on the whole: on a Mac loaded by other builds the
/// reel took over the 300 s this used to allow, while still moving.
const STALL: Duration = Duration::from_secs(180);

/// Waits for the first source's analysis by its progress: panics when it fails, or when
/// nothing moves for [`STALL`].
fn wait_analysed(c: &AppCore, pid: &str) {
    let mut last = (String::new(), -1.0);
    let mut moved = Instant::now();
    loop {
        let st = gmclip_app::analysis::status(c, pid);
        let s0 = &st["sources"][0];
        if let Some(pr) = c.analysis.progress(pid, s0["source"].as_str().unwrap()) {
            match pr.phase.as_str() {
                "done" => return,
                "failed" => panic!("the analysis failed: {st}"),
                _ => {}
            }
            let now = (pr.phase.clone(), pr.done_s);
            if now != last {
                last = now;
                moved = Instant::now();
            }
        } else if s0["phase"] == "done" {
            return;
        }
        assert!(
            moved.elapsed() < STALL,
            "the analysis has not moved for {STALL:?}: {st}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn columns_on_the_output_clock() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-columns-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = core_at(&root);
    let reads = gmclip_app::analysis::whole_track_reads(Path::new(video()));
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    wait_analysed(&c, &pid);
    let pl = c.planned(&pid, &cut).unwrap();
    let frames = pl.plan.output.frames as u32;
    // the columns' analysis took the voice the ASR decoded: the track was not read again
    // (only when the ASR ran: without its model, the analysis reads the track itself)
    let st = gmclip_app::analysis::status(&c, &pid);
    if st["sources"][0]["asr_error"].is_null() {
        assert_eq!(
            gmclip_app::analysis::whole_track_reads(Path::new(video())),
            reads,
            "the analysis read the voice track again"
        );
    }

    // header and one level per output frame
    let pk = get(&c, &pid, &cut, "peaks");
    assert_eq!((pk.version, pk.kind, pk.count), (1, 4, frames));
    assert_eq!(pk.rev, pl.rev);
    assert_eq!(pk.body.len(), frames as usize * 8);
    let p = f32s(&pk.body);
    assert!(
        p.chunks(2)
            .all(|m| m[0] <= m[1] && (-1.0..=1.0).contains(&m[0]))
    );
    let en = get(&c, &pid, &cut, "energy");
    assert_eq!((en.kind, en.count), (5, frames));
    let db = f32s(&en.body);
    let loud = db.iter().filter(|d| **d > -40.0).count();
    assert!(loud * 2 > db.len(), "speech over most of the reel: {loud}");
    // quiet regions: inside the cut, in order, where the level is low
    let q = get(&c, &pid, &cut, "quiet");
    assert_eq!(q.kind, 6);
    let regs = u32s(&q.body);
    assert_eq!(regs.len(), q.count as usize * 2);
    for r in regs.chunks(2) {
        assert!(r[0] < r[1] && r[1] <= frames, "{r:?}");
    }
    // words: the strip's played words, same frames
    let strip = gmclip_app::words::cut_words(&c, &pid, &cut).unwrap();
    let played: Vec<(u32, u32)> = strip["words"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["struck"] == false)
        .map(|w| {
            (
                w["i"].as_u64().unwrap() as u32,
                w["frame"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    let w = get(&c, &pid, &cut, "words");
    assert_eq!(w.kind, 2);
    let rows = word_rows(&w);
    eprintln!(
        "h79: {frames} frames, {} quiet regions, {} words",
        q.count,
        rows.len()
    );
    if !played.is_empty() {
        assert_eq!(rows.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>(), played);
        assert!(rows.iter().all(|r| !r.2.is_empty()));
    }

    // an edit: the part trimmed to 40–50 s and a part added over 5–15 s; the columns follow
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let part = doc
        .iter::<gmclip_model::entities::Part>()
        .next()
        .unwrap()
        .clone();
    let media = serde_json::to_value(part.media).unwrap();
    c.call(
        &pid,
        "part_trim",
        json!({"part": part.id.0.to_string(), "src_in": 40 * FLICKS, "src_out": 50 * FLICKS}),
    )
    .unwrap();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "media": media, "src_in": 5 * FLICKS, "src_out": 15 * FLICKS}),
    )
    .unwrap();
    let pl2 = c.planned(&pid, &cut).unwrap();
    let en2 = get(&c, &pid, &cut, "energy");
    assert_eq!(en2.count as u64, pl2.plan.output.frames);
    assert_eq!(en2.rev, pl2.rev);
    let db2 = f32s(&en2.body);
    // each output frame carries the level of the source frame it plays (the plan's map)
    let orig = &pl.plan.parts[0];
    eprintln!(
        "edited: {:?}",
        pl2.plan
            .parts
            .iter()
            .map(|p| (p.out.start(), p.out.end(), p.picture.first_frame))
            .collect::<Vec<_>>()
    );
    let mut checked = 0;
    for p in &pl2.plan.parts {
        for k in [p.out.start(), p.out.start() + 7, p.out.end() - 1] {
            let src = p.picture.first_frame + (k - p.out.start()) - orig.picture.first_frame
                + orig.out.start();
            assert_eq!(db2[k as usize], db[src as usize], "frame {k}");
            checked += 1;
        }
    }
    assert_eq!(checked, 6);
    let rows2 = word_rows(&get(&c, &pid, &cut, "words"));
    assert!(rows2.windows(2).all(|r| r[0].1 <= r[1].1), "output order");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The 3 h timing (`cargo test --test columns -- --ignored --nocapture`): the analysis
/// that makes the columns' blobs, decoded and computed from (a)'s voice track; the
/// columns' own build for a 3 h source from the voice its transcription decoded (the
/// read the two now share), gated at 30 s; and the four columns for a 3 h cut of (a),
/// cold (blobs decoded) and warm.
#[test]
#[ignore = "decodes two hours of audio; run for the perf row"]
fn three_hour_columns() {
    if gmclip_model::lab::skip_without([long()]) {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-columns3h-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = core_at(&root);
    let v = gmclip_app::video::open_video(&c, Path::new(long())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.cancel_project(&pid);
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let src = doc
        .iter::<gmclip_model::entities::Source>()
        .next()
        .unwrap()
        .clone();
    let secs = src.probe.duration.0 as f64 / FLICKS as f64;
    let audio = gmclip_core::host::AudioSource {
        path: src.path.clone().into(),
        track: src.audio_roles.voice,
        fingerprint: src.fingerprint.clone(),
    };
    let t = Instant::now();
    let blobs = c
        .analysis
        .host()
        .analyze(&audio, &std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    let analyse = t.elapsed().as_secs_f64();
    let owner = src.id.0.to_string();
    for b in blobs {
        p.handle
            .put_analysis(&owner, &b.kind, &b.revision, b.body)
            .unwrap();
    }
    // the columns' own build (decode + the DSP they read), and that DSP on a 3 h buffer
    let tb = gmclip_app::columns::time_build(
        &c,
        &pid,
        Path::new(&src.path),
        src.audio_roles.voice,
        src.probe.duration.0,
        3.0,
    );
    // a 3 h cut: the whole source again and again
    let media = json!({"source": owner});
    // on the frame grid: the opened cut's own part covers the whole video
    let dur = doc
        .iter::<gmclip_model::entities::Part>()
        .next()
        .unwrap()
        .src_out
        .0;
    let fpf = FLICKS / 30;
    let mut left = 3 * 3600 * FLICKS - dur;
    while left > FLICKS {
        let n = left.min(dur) / fpf * fpf;
        c.call(
            &pid,
            "part_add",
            json!({"cut": cut, "media": media, "src_in": 0, "src_out": n}),
        )
        .unwrap();
        left -= n;
    }
    let pl = c.planned(&pid, &cut).unwrap();
    let out_h = pl.plan.output.frames as f64 / 30.0 / 3600.0;
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    for kind in ["peaks", "energy", "quiet", "words"] {
        let t = Instant::now();
        let col = get(&c, &pid, &cut, kind);
        cold.push((kind, t.elapsed().as_secs_f64() * 1000.0, col.body.len()));
        let t = Instant::now();
        get(&c, &pid, &cut, kind);
        warm.push((kind, t.elapsed().as_secs_f64() * 1000.0));
    }
    eprintln!(
        "source {:.2} h; analysis (decode + peaks + energy x3 + quiet grid + VAD) {analyse:.1} s \
         = {:.1} s per hour → {:.1} s for 3 h; cut {out_h:.2} h ({} frames)",
        secs / 3600.0,
        analyse / (secs / 3600.0),
        analyse / (secs / 3600.0) * 3.0,
        pl.plan.output.frames
    );
    eprintln!("columns' build: {tb}");
    // the gate: a 3 h source's columns after its transcription, which read the voice
    // track the columns are made from (the read is shared, D.02)
    let built = tb["columns_s"].as_f64().expect("columns made") + tb["store_s"].as_f64().unwrap();
    assert!(built <= 30.0, "columns for 3 h took {built:.1} s");
    eprintln!("columns cold (ms, bytes): {cold:?}");
    eprintln!("columns warm (ms): {warm:?}");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
