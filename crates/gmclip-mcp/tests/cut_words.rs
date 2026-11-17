//! Decision 0040 (AJ: "a lot of spaces and some sentences dont make sense and also a lot
//! of repeats"): an agent reads what a cut says now (`cut_get{projection: words}`),
//! lint hears dead air, retakes and mid-sentence edges, `sweep_retakes` strikes the
//! earlier attempts (preview, then apply), a strike naming words with a hole is refused,
//! and `verify` without an export says what to do instead.

mod common;

use common::{SEC, World, code};
use serde_json::{Value, json};

/// AJ's bb-mobile2 opening, then a 3 s pause and a sentence (source seconds).
fn words() -> Vec<(String, f64, f64)> {
    let said =
        "Alright, so when I first went to Okay, when I first tried to install the mobile app.";
    let mut t = 200.0;
    let mut v: Vec<(String, f64, f64)> = said
        .split_whitespace()
        .map(|w| {
            let a = t;
            t += 0.4;
            (w.to_owned(), a, a + 0.35)
        })
        .collect();
    t += 3.0;
    for w in "It connected great.".split_whitespace() {
        v.push((w.to_owned(), t, t + 0.35));
        t += 0.4;
    }
    v
}

fn texts(v: &Value) -> String {
    v["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"]["text"].as_str().unwrap_or("").to_owned())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_hear_the_cut_sweep_retakes_and_lint_what_is_left() {
    let w = World::start().await;
    let fx = w.fixture("p.gmclip", 1, "p");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let ov = c.ok("project_overview", json!({})).await;
    let source = ov["sources"][0]["id"].as_str().unwrap().to_owned();
    let ws = words();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": "fake-asr@1", "untrusted": true,
        "words": ws.iter().map(|(t, a, b)| json!({"text": t, "start": (a * SEC as f64) as i64, "end": (b * SEC as f64) as i64})).collect::<Vec<_>>()});
    w.holder
        .resolve(None)
        .unwrap()
        .handle
        .put_analysis(
            &source,
            "words",
            "fake-asr@1",
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
    // the cut plays the whole passage (the fixture's own part is wordless: delete it)
    let as_of = c.ok("project_overview", json!({})).await["as_of"].clone();
    c.ok(
        "cut_edit",
        json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [
            {"op": "part_add", "media": {"source": source}, "src_in": 200 * SEC, "src_out": 212 * SEC},
            {"op": "part_delete", "part": fx.parts[0]}]}),
    )
    .await;

    // what it says now: the words, the silence, the retake
    let r = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "words"}))
        .await;
    assert_eq!(r["projection"], "words");
    assert_eq!(r["retakes"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["retakes"][0]["kind"], "restarted");
    let notes = r["parts"][0]["notes"].to_string();
    assert!(notes.contains("s silent at"), "{notes}");
    assert!(r["parts"][0]["text"]["untrusted"] == true);
    let display = r["display"].as_str().unwrap();
    assert!(display.contains("1 retake(s) left"), "{display}");
    assert!(
        display.contains("<untrusted>"),
        "transcript text is marked: {display}"
    );
    // the words carry their ids for strikes
    assert_eq!(r["parts"][0]["words"][0][0], 0);

    // lint hears it too
    let l = c.ok("lint", json!({"cut": fx.cut})).await;
    let rules: Vec<&str> = l["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"retake"), "{l}");
    assert!(rules.contains(&"dead_air"), "{l}");

    // sweep_retakes: a preview naming both attempts, then applied under the base
    let p = c
        .ok(
            "command_run",
            json!({"command": "sweep_retakes", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(p["kind"], "retakes");
    // the transcripts' fingerprint, not their revisions spelt out (a sweep records ≤ 300)
    assert!(p["revision"].as_str().unwrap().len() <= 64, "{p}");
    assert_eq!(p["dry_run"], true);
    assert_eq!(p["cuts"].as_array().unwrap().len(), 1, "{p}");
    assert!(
        p["cuts"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("restarted")
    );
    let applied = c
        .ok(
            "command_run",
            json!({"command": "sweep_retakes", "base": {"as_of": p["as_of"]}, "args": {"cut": fx.cut, "dry_run": false}}),
        )
        .await;
    assert!(applied["committed"].as_u64().is_some(), "{applied}");
    let r = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "words"}))
        .await;
    assert_eq!(
        texts(&r),
        "Alright, so | when I first tried to install the mobile app. It connected great.",
        "the earlier attempt went, the kept one plays: {r}"
    );
    assert_eq!(r["retakes"].as_array().unwrap().len(), 0);
    // the strike joined two pieces of one sentence: not flagged as mid-sentence
    let l = c.ok("lint", json!({"cut": fx.cut})).await;
    assert!(!l["findings"].to_string().contains("mid_sentence"), "{l}");

    // striking a part's only words takes its wordless rest too: no sliver of silence
    // is left as a part (the first part is "Alright, so" before the struck attempt)
    let as_of = r["as_of"].clone();
    let w0 = |i: usize| {
        let (t, a, b) = &ws[i];
        json!({"word_id": {"asr_revision": "fake-asr@1", "ordinal": i}, "t_src_start": (a * SEC as f64) as i64,
               "t_src_end": (b * SEC as f64) as i64, "normalised_text": t.to_lowercase()})
    };
    let s = c
        .ok(
            "cut_edit",
            json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [
                {"op": "words_strike", "cut": fx.cut, "words": [w0(0), w0(1)]}]}),
        )
        .await;
    assert_eq!(s["output"]["results"][0]["parts_removed"], 1, "{s}");
    let r = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "words"}))
        .await;
    assert_eq!(
        texts(&r),
        "when I first tried to install the mobile app. It connected great."
    );

    // a strike naming words with a hole is refused (it would take the hole too)
    let word = |i: usize| {
        let (t, a, b) = &ws[i];
        json!({"word_id": {"asr_revision": "fake-asr@1", "ordinal": i}, "t_src_start": (a * SEC as f64) as i64,
               "t_src_end": (b * SEC as f64) as i64, "normalised_text": t.to_lowercase()})
    };
    let as_of = r["as_of"].clone();
    let e = c
        .call(
            "cut_edit",
            json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [
                {"op": "words_strike", "cut": fx.cut, "words": [word(14), word(15), word(17)]}]}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Invalid", "{e}");
    assert!(
        e["reason"]
            .as_str()
            .unwrap()
            .contains("every word of one span"),
        "{e}"
    );

    // a soundless clip (a graphic) played between sentences is a silent part
    // (added by the human: source_add is not an agent's)
    let gfx = w
        .holder
        .resolve(None)
        .unwrap()
        .handle
        .call(gmclip_core::CallRequest {
            command: "source_add".into(),
            args: json!({"path": "/m/graphic.mp4", "fingerprint": "fp2",
                         "probe": {"duration": 10 * SEC, "tracks": [], "audio_tracks": []}}),
            base: None,
            request_id: None,
            caller: gmclip_core::Caller::human(),
        })
        .unwrap();
    let gfx_id = gfx.output["id"].as_str().unwrap().to_owned();
    let as_of = c.ok("project_overview", json!({})).await["as_of"].clone();
    c.ok(
        "cut_edit",
        json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [
            {"op": "part_add", "media": {"source": gfx_id}, "src_in": 0, "src_out": 5 * SEC}]}),
    )
    .await;
    let l = c.ok("lint", json!({"cut": fx.cut})).await;
    assert!(l["findings"].to_string().contains("silent_part"), "{l}");
    let r = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "words"}))
        .await;
    assert!(r["display"].as_str().unwrap().contains("no sound"), "{r}");

    // verify without an export says what to do
    let e = c
        .call("verify", json!({"cut": fx.cut, "kind": "joins"}))
        .await
        .unwrap_err();
    assert!(
        e["reason"]
            .as_str()
            .unwrap()
            .contains("export the cut first"),
        "{e}"
    );
}
