//! `see`, `export`, `verify` and `job_wait` through the agent surface, with a test
//! planner and a fake renderer (the real renderer is tested in gmclip-cli's host).

mod common;

use std::sync::{Arc, Mutex};

use common::{World, code};
use gmclip_core::gmclip_plan::Plan;
use gmclip_core::host::{ExportDone, HostError, SeeImage, SeeLayout, SeeRequest};
use gmclip_core::{CmdError, MediaRender, Planner};
use gmclip_mcp::client::RunCredential;
use gmclip_mcp::holder::{Control, LaunchRegister};
use gmclip_mcp::wire;
use gmclip_model::entities::*;
use gmclip_model::{Doc, Id};
use serde_json::{Value, json};

/// One part per cut, from the cut's first part, as a valid RenderPlan.
#[derive(Debug)]
struct TestPlanner;

impl Planner for TestPlanner {
    fn plan(&self, doc: &Doc, cut: Id<Cut>, _framing: Id<Framing>) -> Result<Plan, CmdError> {
        let p = doc
            .children::<Part>(cut.entity())
            .next()
            .and_then(|p| doc.get(p))
            .ok_or(CmdError::NotFound)?;
        let MediaRef::Source(sid) = p.media else {
            return Err(CmdError::NotFound);
        };
        let src = doc.get(sid).ok_or(CmdError::NotFound)?;
        let secs = (p.src_out.0 - p.src_in.0) as f64 / 705_600_000.0;
        let frames = (secs * 30.0).round() as u64;
        let v = json!({
            "schema": "gmclip.renderplan/0",
            "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1},
                "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                          "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
                "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}, "frames": frames},
            "background": {"kind": "colour", "rgb": "#000000"},
            "sources": [{"id": sid.0.to_string(), "path": src.path}],
            "parts": [{"id": "p1", "out": [0, frames], "picture": {"source": sid.0.to_string(), "first_frame": 0}, "audio": {"source": sid.0.to_string(), "first_sample": 0, "rate": 48000},
                       "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": 640, "h": 360}, "place": {"x": 0, "y": 656, "w": 1080, "h": 607.5}, "z": 0}]}],
            "audio": {"loudness": {"mode": "as_recorded"}},
        });
        gmclip_core::gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).map_err(|_| {
            CmdError::Internal {
                trace_id: "test-plan".into(),
            }
        })
    }
}

#[derive(Debug, Default)]
struct FakeMedia {
    seen: Mutex<Vec<SeeRequest>>,
    panic_verify: std::sync::atomic::AtomicBool,
    /// Stored assets by sha256 (a recording's input track).
    assets: Mutex<Vec<(String, Vec<u8>)>>,
    /// Frame reads asked for (the input-log path reads none).
    luma_reads: std::sync::atomic::AtomicUsize,
}

impl MediaRender for FakeMedia {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn see(&self, _: &Plan, req: &SeeRequest) -> Result<SeeImage, HostError> {
        self.seen.lock().unwrap().push(req.clone());
        Ok(SeeImage {
            png: b"\\x89PNG fake".to_vec(),
            width: req.max_px,
            height: req.max_px / 2,
            notes: vec![],
        })
    }
    fn export(
        &self,
        plan: &Plan,
        out: &std::path::Path,
        _: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ExportDone, HostError> {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(out, b"mp4").unwrap();
        Ok(ExportDone {
            out: out.to_owned(),
            frames: plan.output.frames,
            seconds: 2.0,
            notes: vec![],
        })
    }
    fn verify(&self, _: &Plan, out: &std::path::Path) -> Result<Value, HostError> {
        if self.panic_verify.load(std::sync::atomic::Ordering::Relaxed) {
            panic!("verify blew up");
        }
        Ok(json!({"passed": out.exists(), "checks": []}))
    }
    fn matte_quality(&self, plan: &Plan, build: bool) -> Result<Value, HostError> {
        Ok(
            json!({"parts": [{"plan_part": plan.parts[0].id.as_str(), "layer": "cam",
            "status": if build { "accurate" } else { "missing" }, "quality": {"score": 42.0}}]}),
        )
    }
    fn asset_bytes(&self, a: &Asset) -> Result<Vec<u8>, HostError> {
        self.assets
            .lock()
            .unwrap()
            .iter()
            .find(|(h, _)| *h == a.sha256)
            .map(|(_, b)| b.clone())
            .ok_or(HostError::Failed("no such asset".into()))
    }
    /// 160×90 frames: a flickering box in the top-right corner (typing there).
    fn luma_frames(
        &self,
        _: &Plan,
        _: &str,
        frames: &[u64],
        _: u32,
    ) -> Result<Vec<gmclip_core::activity::LumaFrame>, HostError> {
        self.luma_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(frames
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let mut data = vec![30u8; 160 * 90];
                for y in 10..20 {
                    for x in 120..140 {
                        data[y * 160 + x] = if i % 2 == 0 { 200 } else { 40 };
                    }
                }
                gmclip_core::activity::LumaFrame {
                    width: 160,
                    height: 90,
                    data,
                }
            })
            .collect())
    }
}

/// A planner as the app's is on a cut with heard captions: it plans only with the
/// project's transcripts (`plan_with`); `plan` alone refuses.
#[derive(Debug)]
struct HeardPlanner;

impl Planner for HeardPlanner {
    fn plan(&self, _: &Doc, _: Id<Cut>, _: Id<Framing>) -> Result<Plan, CmdError> {
        Err(CmdError::Unsupported {
            platform: "planner".into(),
            reason: "heard captions need the project's transcripts".into(),
        })
    }
    fn plan_with(
        &self,
        doc: &Doc,
        cut: Id<Cut>,
        framing: Id<Framing>,
        _: &gmclip_core::analysis::AnalysisView,
    ) -> Result<Plan, CmdError> {
        TestPlanner.plan(doc, cut, framing)
    }
}

/// QA (milestones/m2/qa-e2e.md, W6): a short with captions on plans only with its
/// transcripts; "Check quality" and "Follow the action" said nothing (the app's toast
/// came and went) because they planned without them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matte_quality_and_follow_plan_with_the_transcripts() {
    let media = Arc::new(FakeMedia::default());
    let w = World::start_with(Some((
        Arc::new(HeardPlanner),
        media.clone() as Arc<dyn MediaRender>,
    )))
    .await;
    let fx = w.fixture("heard.gmclip", 1, "heard");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let q = c
        .ok(
            "command_run",
            json!({"command": "matte_quality", "args": {"cut": fx.cut, "build": true}}),
        )
        .await;
    assert_eq!(q["parts"][0]["status"], "accurate", "{q}");
    let pv = c
        .ok(
            "command_run",
            json!({"command": "crop_follow_action", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(pv["parts"][0]["crops"].as_array().unwrap().len(), 1, "{pv}");
}

/// [`TestPlanner`], refusing a cut with a text overlay whose font is `missing`, as the
/// model planner refuses a caption without a style.
#[derive(Debug)]
struct PickyPlanner;

impl Planner for PickyPlanner {
    fn plan(&self, doc: &Doc, cut: Id<Cut>, framing: Id<Framing>) -> Result<Plan, CmdError> {
        for o in doc.children::<Overlay>(cut.entity()) {
            if doc
                .get(o)
                .and_then(|o| o.text.as_ref())
                .is_some_and(|t| t.style.font == "missing")
            {
                return Err(CmdError::Unsupported {
                    platform: "planner".into(),
                    reason: "a text overlay's font is not there".into(),
                });
            }
        }
        TestPlanner.plan(doc, cut, framing)
    }
}

/// Decision 0053: `see` renders a cut the planner refuses one overlay of without it, and
/// says which and why; the overlay that plans stays in; export still refuses the cut.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn see_leaves_out_what_the_planner_refuses_and_says_so() {
    let media = Arc::new(FakeMedia::default());
    let w = World::start_with(Some((
        Arc::new(PickyPlanner),
        media.clone() as Arc<dyn MediaRender>,
    )))
    .await;
    let fx = w.fixture("picky.gmclip", 2, "picky");
    let c = launched(&w, &fx, true).await;
    let as_of = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    let text = |font: &str| {
        json!({"op": "overlay_add", "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 1000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9},
            "text": {"runs": [{"text": "hello"}], "style": {"font": font, "size": 80.0, "fill": "#ffffff", "outline": null, "shadow": null, "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}}})
    };
    let r = c
        .ok(
            "cut_edit",
            json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [text("f"), text("missing")]}),
        )
        .await;
    let as_of = r["as_of"].as_u64().unwrap();
    let bad = r["output"]["results"][1]["id"].as_str().unwrap().to_owned();
    let seen = c
        .ok(
            "see",
            json!({"target": format!("cut:{}", fx.cut), "at": [0.0]}),
        )
        .await;
    assert_eq!(seen["frames"], json!([0]), "{seen}");
    assert_eq!(
        seen["skipped"],
        json!([{"element": "overlay", "id": bad, "why": "a text overlay's font is not there"}]),
        "{seen}"
    );
    assert!(
        seen["display"]
            .as_str()
            .unwrap()
            .contains("without 1 element"),
        "{seen}"
    );
    assert_eq!(media.seen.lock().unwrap().len(), 1, "rendered");
    // what export accepts is unchanged
    let e = c
        .call("export", json!({"cut": fx.cut, "base": {"as_of": as_of}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Unsupported", "{e}");
}

async fn world() -> (World, Arc<FakeMedia>) {
    let media = Arc::new(FakeMedia::default());
    let w = World::start_with(Some((
        Arc::new(TestPlanner),
        media.clone() as Arc<dyn MediaRender>,
    )))
    .await;
    (w, media)
}

async fn launched(w: &World, fx: &common::Fx, media: bool) -> common::Client {
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let mut reg = LaunchRegister {
        project: fx.project.clone(),
        cut: fx.cut.clone(),
        credential: wire::hex(&[6u8; 32]),
        wall_clock_s: 600,
        media,
        private_regions: false,
        code: None,
        brief_sha256: Some("0".repeat(64)),
        scopes: None,
        full_access: false,
        grant: None,
    };
    if media {
        let args =
            json!({"project": fx.project, "cut": fx.cut, "media": true, "private_regions": false});
        reg.code = Some(w.owner_code("launch_grant", &args).await);
    }
    let r = w.control(&Control::LaunchRegister(reg)).await;
    let run = r["result"]["run"].as_str().unwrap().to_owned();
    w.client("claude-code", Some(&RunCredential::new(run, vec![6u8; 32])))
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn see_renders_frames_on_the_cut_clock_as_an_image() {
    let (w, media) = world().await;
    let fx = w.fixture("s.gmclip", 2, "see");
    let c = launched(&w, &fx, true).await;
    let r = c
        .svc
        .call_tool(
            rmcp::model::CallToolRequestParams::new("see").with_arguments(
                json!({"target": format!("cut:{}", fx.cut), "at": [0.0, 1.0]})
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true), "{r:?}");
    assert!(
        r.content
            .iter()
            .any(|b| b.as_image().is_some_and(|i| i.mime_type == "image/png")),
        "an image block"
    );
    let text: Value = serde_json::from_str(&r.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(text["frames"], json!([0, 30]));
    assert!(text["note"].as_str().unwrap().contains("untrusted"));
    assert!(
        std::path::Path::new(text["image"]["path"].as_str().unwrap()).exists(),
        "written to the agent-shots cache"
    );
    let req = media.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(req.layout, SeeLayout::Strip);
    assert_eq!(req.max_px, 1568);
    // limits
    assert_eq!(
        code(
            &c.call(
                "see",
                json!({"target": format!("cut:{}", fx.cut), "max_px": 2000})
            )
            .await
            .unwrap_err()
        ),
        "Invalid"
    );
    assert_eq!(
        code(
            &c.call(
                "see",
                json!({"target": format!("cut:{}", fx.cut), "at": [99.0]})
            )
            .await
            .unwrap_err()
        ),
        "Invalid"
    );
    assert_eq!(
        code(
            &c.call("see", json!({"target": format!("cut:{}", fx.other_cut)}))
                .await
                .unwrap_err()
        ),
        "Unauthorized"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_runs_as_a_job_verifies_and_respects_lint_and_base() {
    let (w, _) = world().await;
    let fx = w.fixture("e.gmclip", 2, "export");
    let c = launched(&w, &fx, false).await;
    let as_of = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    // an agent's number with no provenance: an error-level lint blocks the agent's export
    let ov = c
        .ok("cut_edit", json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [{"op": "overlay_add", "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 1000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9},
            "text": {"runs": [{"text": "67 seconds"}], "style": {"font": "f", "size": 80.0, "fill": "#ffffff", "outline": null, "shadow": null, "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}}}]}))
        .await;
    let as_of = ov["as_of"].as_u64().unwrap();
    let ov_id = ov["output"]["results"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = c
        .call("export", json!({"cut": fx.cut, "base": {"as_of": as_of}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NeedsApproval", "{e}");
    assert_eq!(
        e["plan"]["blocked_by"][0]["rule"],
        "number_without_provenance"
    );
    // remove it (a note-only provenance would not do), then a stale base conflicts and a fresh one exports
    let fixed = c
        .ok("cut_edit", json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [{"op": "overlay_delete", "overlay": ov_id}]}))
        .await;
    assert_eq!(
        code(
            &c.call("export", json!({"cut": fx.cut, "base": {"as_of": as_of}}))
                .await
                .unwrap_err()
        ),
        "Conflict"
    );
    let job = c
        .ok(
            "export",
            json!({"cut": fx.cut, "base": {"as_of": fixed["as_of"]}}),
        )
        .await;
    let id = job["job"].as_str().unwrap().to_owned();
    assert!(
        job["out"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .contains("/exports/export-"),
        "{job}"
    );
    let waited = c
        .ok(
            "command_run",
            json!({"command": "job_wait", "args": {"job": id, "timeout_s": 10}}),
        )
        .await;
    assert_eq!(waited["state"], "done", "{waited}");
    assert_eq!(
        waited["result"]["verify"]["passed"], true,
        "verified automatically"
    );
    let v = c
        .ok(
            "verify",
            json!({"cut": fx.cut, "kind": "export", "export": id}),
        )
        .await;
    assert_eq!(v["report"]["passed"], true);
    // joins: lane D's ASR (the fake renderer has no dev path)
    assert_eq!(
        code(
            &c.call(
                "verify",
                json!({"cut": fx.cut, "kind": "joins", "export": id})
            )
            .await
            .unwrap_err()
        ),
        "Unsupported"
    );
    assert_eq!(
        code(
            &c.call(
                "command_run",
                json!({"command": "job_wait", "args": {"job": "nope"}})
            )
            .await
            .unwrap_err()
        ),
        "NotFound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_planner_see_and_export_say_so() {
    let w = World::start().await;
    let fx = w.fixture("n.gmclip", 1, "n");
    let c = launched(&w, &fx, true).await;
    let e = c
        .call("see", json!({"target": format!("cut:{}", fx.cut)}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Unsupported");
    assert!(e.to_string().contains("A18"), "{e}");
}

#[test]
fn private_regions_are_masked_through_crop_and_place_and_unknown_sources_fail_closed() {
    use gmclip_model::fixtures as f;
    use gmclip_model::geometry::NormRect;
    let mut d = Doc::new();
    let mut src = f::source(1);
    src.probe.tracks = vec![TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(640),
        height: Some(360),
        fps: None,
        timescale: 15360,
        gop_max_s: None,
    }];
    let sid = src.id;
    d.insert_for_test(src);
    d.insert_for_test(Region {
        id: Id::new(gmclip_model::Ulid(90)),
        source: sid,
        role: RegionRole::Chat,
        rect: NormRect {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        },
        from: RegionFrom::Human,
    });
    let plan = TestPlanner
        .plan(
            &{
                let mut d2 = d.clone();
                let c = f::cut(2);
                let cut = c.id;
                d2.insert_for_test(c);
                d2.insert_for_test(f::part(
                    3,
                    cut,
                    gmclip_model::order::OrderKey::first(),
                    0,
                    2 * 705_600_000,
                ));
                if let Some(mut p) = d2.get(Id::<Part>::new(gmclip_model::Ulid(3))).cloned() {
                    p.media = MediaRef::Source(sid);
                    d2.insert_for_test(p);
                }
                d2
            },
            Id::new(gmclip_model::Ulid(2)),
            Id::new(gmclip_model::Ulid(4)),
        )
        .unwrap();
    // chat = the source's top-right quarter → the place's top-right quarter
    let m = gmclip_mcp::media::masks(&d, &plan, 0);
    assert_eq!(m.len(), 1);
    assert_eq!((m[0].x, m[0].y, m[0].w, m[0].h), (540, 656, 540, 304));
    assert!(m[0].label.contains("chat"));
    // a plan source that is not a model source: the whole layer is masked
    let mut other = plan.clone();
    other.parts[0].picture.source =
        gmclip_core::gmclip_plan::scalars::Id::new("not-a-source").unwrap();
    let m = gmclip_mcp::media::masks(&d, &other, 0);
    assert_eq!((m[0].x, m[0].y, m[0].w, m[0].h), (0, 656, 1080, 608));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_jobs_are_robust_private_and_idempotent() {
    let (w, media) = world().await;
    let fx = w.fixture("j.gmclip", 1, "jobs");
    let c = launched(&w, &fx, false).await;
    let as_of = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    // a panic in the automatic verify fails the job and frees the slot
    media
        .panic_verify
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let j1 = c
        .ok("export", json!({"cut": fx.cut, "base": {"as_of": as_of}}))
        .await["job"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = c
        .ok(
            "command_run",
            json!({"command": "job_wait", "args": {"job": j1, "timeout_s": 10}}),
        )
        .await;
    assert_eq!(r["state"], "failed", "{r}");
    media
        .panic_verify
        .store(false, std::sync::atomic::Ordering::Relaxed);
    // a repeated request id returns the same job
    let a = c
        .ok(
            "export",
            json!({"cut": fx.cut, "base": {"as_of": as_of}, "request_id": "x1"}),
        )
        .await;
    let b = c
        .ok(
            "export",
            json!({"cut": fx.cut, "base": {"as_of": as_of}, "request_id": "x1"}),
        )
        .await;
    assert_eq!(a["job"], b["job"]);
    assert_eq!(b["replayed"], true);
    let j2 = a["job"].as_str().unwrap().to_owned();
    assert_eq!(
        c.ok(
            "command_run",
            json!({"command": "job_wait", "args": {"job": j2, "timeout_s": 10}})
        )
        .await["state"],
        "done"
    );
    // another identity sees nothing of it
    let other = w.client("someone-else", None).await;
    assert_eq!(
        code(
            &other
                .call(
                    "command_run",
                    json!({"command": "job_wait", "args": {"job": j2}})
                )
                .await
                .unwrap_err()
        ),
        "NotFound"
    );
    assert_eq!(
        code(
            &other
                .call(
                    "verify",
                    json!({"cut": fx.cut, "kind": "export", "export": j2})
                )
                .await
                .unwrap_err()
        ),
        "NotFound"
    );
    // a deletion in the cut after the base is a conflict
    let now = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    let ov = c
        .ok("cut_edit", json!({"cut": fx.cut, "base": {"as_of": now}, "ops": [{"op": "overlay_add", "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 500}, "place": {"x": 0.5, "y": 0.2, "w": 0.5}}]}))
        .await;
    let before_delete = ov["as_of"].as_u64().unwrap();
    let ov_id = ov["output"]["results"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.ok("cut_edit", json!({"cut": fx.cut, "base": {"as_of": before_delete}, "ops": [{"op": "overlay_delete", "overlay": ov_id}]})).await;
    assert_eq!(
        code(
            &c.call(
                "export",
                json!({"cut": fx.cut, "base": {"as_of": before_delete}})
            )
            .await
            .unwrap_err()
        ),
        "Conflict"
    );
    // a per-entity base must name the cut
    assert_eq!(
        code(
            &c.call(
                "export",
                json!({"cut": fx.cut, "base": {"part:01ARZ3NDEKTSV4RRFFQ69G5FAV": 1}})
            )
            .await
            .unwrap_err()
        ),
        "Invalid"
    );
}

/// Masks through lane A's planner: plan sources are `s1, …`, matched to the model by
/// path (a path two sources share is not matched: the layer is masked whole).
#[test]
fn masks_follow_the_model_planner_s_sources_by_path() {
    use gmclip_core::planner::PlanHost;
    use gmclip_model::fixtures as f;
    use gmclip_model::geometry::NormRect;
    use gmclip_model::order::OrderKey;
    #[derive(Debug)]
    struct NoFonts;
    impl PlanHost for NoFonts {
        fn font_path(&self, _: &Font) -> Option<String> {
            None
        }
    }
    let mut d = Doc::new();
    let mut src = f::source(1);
    src.probe.tracks = vec![TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(640),
        height: Some(360),
        fps: Some(gmclip_model::time::Rational::new(30, 1)),
        timescale: 15360,
        gop_max_s: None,
    }];
    let sid = src.id;
    d.insert_for_test(src);
    d.insert_for_test(Region {
        id: Id::new(gmclip_model::Ulid(90)),
        source: sid,
        role: RegionRole::Chat,
        rect: NormRect {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        },
        from: RegionFrom::Human,
    });
    let c = f::cut(2);
    let cut = c.id;
    d.insert_for_test(c);
    let fr = f::framing(4, cut);
    let fid = fr.id;
    d.insert_for_test(fr);
    let mut l = f::layer(
        5,
        LayerOwner::Framing(fid),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    );
    l.place = gmclip_model::geometry::Place {
        x: 0.0,
        y: 0.3416,
        w: 1.0,
    };
    d.insert_for_test(l);
    let mut p = f::part(3, cut, OrderKey::first(), 0, 2 * 705_600_000);
    p.media = MediaRef::Source(sid);
    d.insert_for_test(p);
    let plan = gmclip_core::planner::plan_cut(&d, cut, fid, &NoFonts).unwrap();
    assert_eq!(
        plan.sources[0].id.as_str(),
        "s1",
        "the planner's own source names"
    );
    let m = gmclip_mcp::media::masks(&d, &plan, 0);
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(m[0].x >= 539 && m[0].w <= 541, "{m:?}");
    // a second model source on the same path: unmapped, the whole layer is masked
    let mut twin = f::source(7);
    twin.path = d.get(sid).unwrap().path.clone();
    d.insert_for_test(twin);
    let m = gmclip_mcp::media::masks(&d, &plan, 0);
    assert_eq!((m[0].x, m[0].w), (0, 1080), "{m:?}");
}

/// R-PROTO-6, per run (decision 0050): a run may have 3 approvals pending; its 4th ask
/// pauses it with `Paused{pending, resume}` ("This agent keeps asking — review run" and
/// where the human resumes it) until a human resolves them. The same refusal again (same
/// cut revision and findings) is the ask already waiting, `export{dry_run}` reports the
/// blockers without asking, and another thread of the same client is never paused with
/// it. Each approval carries the server-computed effects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fourth_approval_pauses_that_run_only() {
    let (w, _) = world().await;
    let fx = w.fixture("ap.gmclip", 2, "approvals");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let sibling = w.client("claude-code", None).await;
    let mut as_of = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    let mut hook = async |n: u32| {
        let ov = c
            .ok("cut_edit", json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [{"op": "overlay_add", "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 1000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9},
                "text": {"runs": [{"text": format!("{n} seconds")}], "style": {"font": "f", "size": 80.0, "fill": "#ffffff", "outline": null, "shadow": null, "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}}}]}))
            .await;
        as_of = ov["as_of"].as_u64().unwrap();
        as_of
    };
    let at = hook(67).await;
    // the same refusal three times is one ask
    let mut ids = vec![];
    for _ in 0..3 {
        let e = c
            .call("export", json!({"cut": fx.cut, "base": {"as_of": at}}))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "NeedsApproval", "{e}");
        ids.push(e["plan"]["approval"].clone());
    }
    assert!(ids.iter().all(|i| *i == ids[0] && i.is_string()), "{ids:?}");
    assert_eq!(w.holder.approvals().len(), 1);
    // a dry run says what blocks it and asks nobody
    let d = c
        .ok("export", json!({"cut": fx.cut, "dry_run": true}))
        .await;
    assert_eq!(d["blocked"], true, "{d}");
    assert_eq!(
        d["blocked_by"][0]["rule"], "number_without_provenance",
        "{d}"
    );
    assert_eq!(w.holder.approvals().len(), 1);
    // and from a new connection of the same agent (one that starts gmclip per call)
    let e = sibling
        .call("export", json!({"cut": fx.cut, "base": {"as_of": at}}))
        .await
        .unwrap_err();
    assert_eq!(e["plan"]["approval"], ids[0], "{e}");
    assert_eq!(w.holder.approvals().len(), 1);
    // two more, each of a changed cut
    for n in [68, 69] {
        let at = hook(n).await;
        let e = c
            .call("export", json!({"cut": fx.cut, "base": {"as_of": at}}))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "NeedsApproval", "{e}");
    }
    let pending = w.holder.approvals();
    assert_eq!(pending.len(), 3);
    assert_eq!(
        pending[0].effects["blocked_by"][0]["rule"], "number_without_provenance",
        "the server's effects, not the agent's words"
    );
    // the 4th: paused, and so is everything else this run asks
    let at = hook(70).await;
    let e = c
        .call("export", json!({"cut": fx.cut, "base": {"as_of": at}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Paused", "{e}");
    assert_eq!(e["pending"], 3, "{e}");
    let resume = e["resume"].as_str().unwrap();
    assert!(
        resume.contains("keeps asking") && resume.contains("Needs you"),
        "{e}"
    );
    let e = c.call("cut_get", json!({"cut": fx.cut})).await.unwrap_err();
    assert_eq!(code(&e), "Paused");
    assert_eq!(w.holder.approvals().len(), 3, "the 4th is not queued");
    // the same client's other thread goes on, asks of its own included
    sibling.ok("cut_get", json!({"cut": fx.cut})).await;
    let e = sibling
        .call("export", json!({"cut": fx.cut, "base": {"as_of": at}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NeedsApproval", "{e}");
    // a human reviews the run: it may ask again
    let run = pending[0].run.clone();
    assert_eq!(w.holder.approval_resolve(None, Some(&run)), 3);
    c.ok("cut_get", json!({"cut": fx.cut})).await;
}

/// R.02 / R.03 through the agent surface: `matte_quality` names the model part behind
/// each plan part; `crop_follow_action` previews, then applies one transaction that
/// crops the screen layer onto where the screen changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matte_quality_and_follow_the_action() {
    let (w, _media) = world().await;
    let fx = w.fixture("follow.gmclip", 1, "follow");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let q = c
        .ok(
            "command_run",
            json!({"command": "matte_quality", "args": {"cut": fx.cut, "build": true}}),
        )
        .await;
    assert_eq!(q["parts"][0]["status"], "accurate");
    assert_eq!(q["parts"][0]["part"], json!(fx.parts[0]));
    let pv = c
        .ok(
            "command_run",
            json!({"command": "crop_follow_action", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(pv["dry_run"], true, "{pv}");
    let crops = pv["parts"][0]["crops"].as_array().unwrap();
    assert_eq!(crops.len(), 1, "{pv}");
    let cr = &crops[0]["crop"];
    // the box at x 120..140, y 10..20 of 160×90 is inside the crop
    let (x, y, cw, ch) = (
        cr["x"].as_f64().unwrap(),
        cr["y"].as_f64().unwrap(),
        cr["w"].as_f64().unwrap(),
        cr["h"].as_f64().unwrap(),
    );
    assert!(
        x <= 0.75 && x + cw >= 0.875 && y <= 10.0 / 90.0 && y + ch >= 20.0 / 90.0,
        "{cr}"
    );
    assert!(crops[0]["holds"].as_f64().unwrap() >= 0.8);
    let as_of = c.ok("project_overview", json!({})).await["as_of"]
        .as_u64()
        .unwrap();
    let ap = c
        .ok(
            "command_run",
            json!({"command": "crop_follow_action", "base": {"as_of": as_of}, "args": {"cut": fx.cut, "dry_run": false}}),
        )
        .await;
    assert!(ap["committed"].as_u64().is_some(), "{ap}");
    let cut = c.ok("cut_get", json!({"cut": fx.cut})).await;
    let look = &cut["parts"][0]["look"][fx.framing.as_str()];
    assert_eq!(
        look["overrides"], 1,
        "the part's screen crop is overridden: {cut}"
    );
}

/// Follow the cursor through the agent surface: `crop_follow_cursor` previews (frames
/// read at 2 fps; this fake screen has a blinking box and no cursor, so no moves), then
/// applies under `base` as one transaction, and `off` applies a removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_the_cursor_is_agent_reachable() {
    let (w, _media) = world().await;
    let fx = w.fixture("cursor.gmclip", 1, "cursor");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let pv = c
        .ok(
            "command_run",
            json!({"command": "crop_follow_cursor", "args": {"cut": fx.cut, "part": fx.parts[0], "max_zoom": 2.5}}),
        )
        .await;
    assert_eq!(pv["dry_run"], true, "{pv}");
    assert!(pv["follow"]["samples"].as_u64().unwrap() >= 2, "{pv}");
    assert_eq!(
        pv["follow"]["found"], 0,
        "a blinking box is not a cursor: {pv}"
    );
    assert_eq!(pv["follow"]["moves"], 0);
    assert_eq!(pv["follow"]["method"], "pixels", "no input log: {pv}");
    let bad = c
        .call(
            "command_run",
            json!({"command": "crop_follow_cursor", "args": {"cut": fx.cut, "part": fx.parts[0], "max_zoom": 9}}),
        )
        .await
        .unwrap_err();
    assert!(bad.to_string().contains("max_zoom"), "{bad}");
    for args in [
        json!({"cut": fx.cut, "part": fx.parts[0], "dry_run": false}),
        json!({"cut": fx.cut, "part": fx.parts[0], "off": true, "dry_run": false}),
    ] {
        let as_of = c.ok("project_overview", json!({})).await["as_of"]
            .as_u64()
            .unwrap();
        let ap = c
            .ok(
                "command_run",
                json!({"command": "crop_follow_cursor", "base": {"as_of": as_of}, "args": args}),
            )
            .await;
        assert!(ap["display"].as_str().unwrap().contains("written"), "{ap}");
    }
}

/// Gives the fixture's source an input track (decision 0045), as an import of a gmclip
/// recording would: an asset holding the track's JSON and `Source.cursor` naming it.
/// The cursor glides across the picture over the first part (100–102 s), the user types
/// into a field from 100.5 s and clicks at 101.5 s.
fn with_input_log(w: &World, fx: &common::Fx, media: &FakeMedia) {
    use gmclip_model::rec::{Click, Focus, InputTrack, Keys};
    use sha2::Digest;
    let mut t = InputTrack::new(1920, 1080, 2.0);
    for i in 0..=120 {
        let f = i as f32 / 120.0;
        t.cursor.push((
            100_000_000 + i * 16_667,
            200.0 + 1300.0 * f,
            200.0 + 700.0 * f,
            u16::MAX,
        ));
    }
    for i in 121..=240 {
        t.cursor
            .push((100_000_000 + i * 16_667, 1500.0, 900.0, u16::MAX));
    }
    t.focus.push(Focus {
        t: 100_400_000,
        rect: Some([800.0, 300.0, 400.0, 40.0]),
        caret: Some([960.0, 310.0, 2.0, 20.0]),
        window: None,
        role: "AXTextField".into(),
        app: "com.apple.Safari".into(),
    });
    for k in 0..5 {
        t.keys.push(Keys {
            t: 100_500_000 + k * 100_000,
            n: 1,
        });
    }
    t.clicks.push(Click {
        t: 101_500_000,
        x: 1500.0,
        y: 900.0,
        button: 0,
        down: true,
        count: 1,
    });
    let bytes = serde_json::to_vec(&t).unwrap();
    let sha: String = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    media
        .assets
        .lock()
        .unwrap()
        .push((sha.clone(), bytes.clone()));
    let db = gmclip_mcp::project::db_path(&w.env.app_data, &fx.project).unwrap();
    let h = gmclip_core::ProjectHandle::open(&db, gmclip_core::OpenOptions::default()).unwrap();
    let mut doc = h.snapshot().doc.clone();
    h.close().unwrap();
    let asset = Asset {
        id: Id::new(gmclip_model::Ulid(7_045)),
        name: "screen-1.input.json".into(),
        sha256: sha,
        size: bytes.len() as u64,
        media_type: gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE.into(),
        probe: None,
    };
    let mut src = doc.iter::<Source>().next().unwrap().clone();
    src.cursor = Some(SourceCursor::new(asset.id, vec![]));
    doc.insert_for_test(asset);
    doc.insert_for_test(src);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
    gmclip_core::write_fresh_store(&db, &doc, &[]).unwrap();
}

/// A gmclip recording's cursor comes from its input log (decision 0045): the follow
/// reads no frames and says `input_log`; `crop_zoom_focus` zooms on the caret while
/// typing and on the click, previews first and applies as one transaction; a source
/// without a log is refused by the zoom.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_and_zoom_from_the_input_log_are_agent_reachable() {
    let (w, media) = world().await;
    let fx = w.fixture("rec.gmclip", 2, "rec");
    let plain = w.fixture("plain.gmclip", 1, "plain");
    with_input_log(&w, &fx, &media);
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let pv = c
        .ok(
            "command_run",
            json!({"command": "crop_follow_cursor", "args": {"cut": fx.cut, "part": fx.parts[0]}}),
        )
        .await;
    let f = &pv["follow"];
    assert_eq!(f["method"], "input_log", "{pv}");
    assert_eq!(f["samples"], 4, "2 s at 2 fps: {pv}");
    assert_eq!(f["found"], 4, "the log knows where the cursor is: {pv}");
    assert!(f["moves"].as_u64().unwrap() >= 1, "{pv}");
    assert_eq!(
        media.luma_reads.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no frames decoded"
    );
    let zv = c
        .ok(
            "command_run",
            json!({"command": "crop_zoom_focus", "args": {"cut": fx.cut, "part": fx.parts[0], "max_zoom": 2}}),
        )
        .await;
    assert_eq!(zv["dry_run"], true, "{zv}");
    let keys = zv["zoom"]["keys"].as_array().unwrap();
    assert_eq!(keys[0]["why"], "caret", "{zv}");
    let crop = &keys[0]["crop"];
    assert!((crop["w"].as_f64().unwrap() - 0.5).abs() < 1e-9, "{zv}");
    let cx = crop["x"].as_f64().unwrap() + 0.25;
    assert!((cx - 961.0 / 1920.0).abs() < 0.01, "on the caret: {zv}");
    assert!(keys.iter().any(|k| k["why"] == "click"), "{zv}");
    let as_of = c.ok("project_overview", json!({})).await["as_of"]
        .as_u64()
        .unwrap();
    let ap = c
        .ok(
            "command_run",
            json!({"command": "crop_zoom_focus", "base": {"as_of": as_of},
                   "args": {"cut": fx.cut, "part": fx.parts[0], "dry_run": false}}),
        )
        .await;
    assert!(ap["committed"].as_u64().is_some(), "{ap}");
    assert!(ap["display"].as_str().unwrap().contains("written"), "{ap}");
    // a source with no log: the zoom has nothing to aim at
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": plain.home.to_str().unwrap()}}),
    )
    .await;
    let bad = c
        .call(
            "command_run",
            json!({"command": "crop_zoom_focus", "project": plain.project, "args": {"cut": plain.cut, "part": plain.parts[0]}}),
        )
        .await
        .unwrap_err();
    assert!(bad.to_string().contains("input log"), "{bad}");
}

/// Decision 0046: `zoom_auto` turns the input log into automatic zoom sections (a `focus`
/// zoom while typing on the caret, a `cursor` zoom on the click), previews first, applies
/// through `section_auto_apply` as one transaction, and replaces only its own sections.
#[tokio::test]
async fn zoom_auto_writes_zoom_sections_from_the_input_log() {
    let (w, media) = world().await;
    let fx = w.fixture("rec.gmclip", 2, "rec");
    with_input_log(&w, &fx, &media);
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let pv = c
        .ok(
            "command_run",
            json!({"command": "zoom_auto", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(pv["dry_run"], true, "{pv}");
    assert_eq!(
        (pv["parts_read"].as_u64(), pv["parts_skipped"].as_u64()),
        (Some(2), Some(0)),
        "{pv}"
    );
    let s = pv["sections"].as_array().unwrap();
    assert_eq!(s.len(), 2, "{pv}");
    // typing from 100.5 s (cut 0.5 s) on the caret, the click at 101.5 s to the part's end
    assert_eq!(s[0]["zoom"]["target"], "focus", "{pv}");
    assert_eq!(s[0]["zoom"]["depth"], 2.0, "{pv}");
    assert_eq!(
        (s[0]["start_ms"].as_i64(), s[0]["end_ms"].as_i64()),
        (Some(500), Some(1500)),
        "{pv}"
    );
    assert_eq!(s[1]["zoom"]["target"], "cursor", "{pv}");
    assert_eq!(
        (s[1]["start_ms"].as_i64(), s[1]["end_ms"].as_i64()),
        (Some(1500), Some(2000)),
        "{pv}"
    );
    // a hand-made zoom, kept
    let as_of = c.ok("project_overview", json!({})).await["as_of"]
        .as_u64()
        .unwrap();
    c.ok(
        "command_run",
        json!({"command": "section_add", "base": {"as_of": as_of},
               "args": {"cut": fx.cut, "start_ms": 2500, "end_ms": 3500, "kind": {"zoom": {"target": {"point": {"x": 0.5, "y": 0.5}}}}}}),
    )
    .await;
    for removed in [0, 2] {
        let as_of = c.ok("project_overview", json!({})).await["as_of"]
            .as_u64()
            .unwrap();
        let ap = c
            .ok(
                "command_run",
                json!({"command": "zoom_auto", "base": {"as_of": as_of},
                       "args": {"cut": fx.cut, "dry_run": false}}),
            )
            .await;
        assert!(ap["committed"].as_u64().is_some(), "{ap}");
        assert_eq!(ap["removed"], removed, "{ap}");
        assert_eq!(ap["ids"].as_array().unwrap().len(), 2, "{ap}");
    }
    let l = c
        .ok(
            "command_run",
            json!({"command": "section_list", "args": {"cut": fx.cut}}),
        )
        .await;
    let listed = l["output"]["sections"].as_array().unwrap();
    assert_eq!(listed.len(), 3, "{l}");
    assert_eq!(
        listed
            .iter()
            .filter(|s| s["kind"]["zoom"]["auto"] == true)
            .count(),
        2,
        "{l}"
    );
}
