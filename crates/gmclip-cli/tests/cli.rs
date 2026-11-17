//! The `gmclip` binary end to end (A-01, A-02): help for every subcommand, one JSON error
//! object with a `trace_id` and a stable exit code for every failure, stubs that return
//! `Unsupported` cleanly, and logs that carry the same `trace_id` and no home paths or
//! caption text.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn repo(rel: &str) -> String {
    format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
}

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gmclip-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str], logs: &Path, envs: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_gmclip"));
    c.args(args)
        .env("GMCLIP_LOG_DIR", logs)
        .env_remove("GMCLIP_LOG");
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().expect("run gmclip")
}

fn gmclip(args: &[&str]) -> Output {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = scratch(&format!("logs-{n}"));
    let out = run(args, &dir, &[]);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// The single JSON object on stderr.
fn err_json(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(text.lines().count(), 1, "stderr is one line: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("stderr is JSON ({e}): {text}"))
}

fn assert_trace_id(v: &Value) -> String {
    let t = v["trace_id"].as_str().expect("trace_id").to_owned();
    assert_eq!(t.len(), 26, "{t}");
    assert!(t.chars().all(|c| c.is_ascii_alphanumeric()), "{t}");
    t
}

#[test]
fn help_and_version_exit_zero() {
    for args in [
        &["--help"][..],
        &["--version"],
        &["render", "--help"],
        &["still", "--help"],
        &["plan-cut", "--help"],
        &["plan", "--help"],
        &["plan", "validate", "--help"],
        &["probe", "--help"],
        &["remux", "--help"],
    ] {
        let out = gmclip(args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert!(!out.stdout.is_empty(), "{args:?}");
    }
    let v = gmclip(&["--version"]);
    assert!(String::from_utf8_lossy(&v.stdout).starts_with("gmclip "));
}

#[test]
fn stubs_return_unsupported_with_exit_3() {
    let plan = repo("schemas/examples/h79-parts-1-3.renderplan.json");
    // `render` and `still` are built (H-10, H-05b): see
    // `render_with_missing_sources_fails_cleanly`.
    let _ = &plan;
    let cases: &[(&[&str], &str)] = &[(&["probe", "in.mp4"], "probe")];
    for (args, step) in cases {
        let out = gmclip(args);
        assert_eq!(out.status.code(), Some(3), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty());
        let v = err_json(&out);
        assert_eq!(v["code"], "Unsupported", "{v}");
        assert_eq!(v["step"], *step, "{v}");
        assert!(
            v["reason"].as_str().unwrap().contains("not built yet"),
            "{v}"
        );
        assert_trace_id(&v);
    }
}

#[test]
fn render_with_missing_sources_fails_cleanly() {
    // A valid plan whose sources do not exist: a JSON error naming the step, a
    // non-zero exit, and nothing written (H-10).
    let text =
        std::fs::read_to_string(repo("schemas/examples/h79-parts-1-3.renderplan.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    for s in v["sources"].as_array_mut().unwrap() {
        s["path"] = serde_json::json!("/nonexistent/gmclip-test/source.mp4");
    }
    let dir = std::env::temp_dir().join(format!("gmclip-cli-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let plan = dir.join("plan.json");
    std::fs::write(&plan, serde_json::to_vec(&v).unwrap()).unwrap();
    let out_mp4 = dir.join("out.mp4");
    let out = gmclip(&[
        "render",
        plan.to_str().unwrap(),
        "-o",
        out_mp4.to_str().unwrap(),
    ]);
    assert_ne!(out.status.code(), Some(0), "{out:?}");
    let e = err_json(&out);
    assert_trace_id(&e);
    assert!(!out_mp4.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remux_refuses_a_file_that_is_not_an_mp4() {
    // B-06 filled `remux`: a JSON file is no MP4, refused at the demux step.
    let plan = repo("schemas/examples/h79-parts-1-3.renderplan.json");
    let out = gmclip(&["remux", &plan, "-o", "out.mp4"]);
    assert_ne!(out.status.code(), Some(0), "{out:?}");
    let v = err_json(&out);
    assert_eq!(v["step"], "demux", "{v}");
    assert_trace_id(&v);
}

#[test]
fn render_validates_the_plan_first() {
    let out = gmclip(&[
        "render",
        &repo("schemas/examples/invalid/aspect-off.json"),
        "-o",
        "x.mp4",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let v = err_json(&out);
    assert_eq!(v["code"], "Invalid");
    assert_eq!(v["path"], "/parts/1/layers/0/place");
}

#[test]
fn plan_validate_accepts_the_examples() {
    for ex in ["h79-parts-1-3", "native-mode", "bench-60-m0"] {
        let out = gmclip(&[
            "plan",
            "validate",
            &repo(&format!("schemas/examples/{ex}.renderplan.json")),
        ]);
        assert!(out.status.success(), "{ex}: {out:?}");
        assert!(out.stderr.is_empty());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], true);
        assert_trace_id(&v);
    }
}

#[test]
fn plan_validate_names_the_path() {
    let out = gmclip(&[
        "plan",
        "validate",
        &repo("schemas/examples/invalid/aspect-off.json"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let v = err_json(&out);
    assert_eq!(v["code"], "Invalid");
    assert_eq!(v["step"], "plan");
    assert_eq!(v["path"], "/parts/1/layers/0/place");
    let u = gmclip(&[
        "plan",
        "validate",
        &repo("schemas/examples/invalid/picture-fps-mismatch.json"),
    ]);
    assert_eq!(u.status.code(), Some(3));
    assert_eq!(err_json(&u)["code"], "Unsupported");
}

#[test]
fn file_and_usage_errors_are_json_too() {
    let missing = gmclip(&["plan", "validate", "/nonexistent/plan.json"]);
    assert_eq!(missing.status.code(), Some(4));
    let v = err_json(&missing);
    assert_eq!(v["code"], "NotFound");
    assert!(
        !v.to_string().contains("nonexistent"),
        "paths are not echoed: {v}"
    );

    let dir = gmclip(&["plan", "validate", &repo("schemas")]);
    assert_eq!(dir.status.code(), Some(5), "{dir:?}");
    assert_eq!(err_json(&dir)["code"], "Io");

    for args in [&["--bogus"][..], &[], &["render"], &["plan", "frobnicate"]] {
        let out = gmclip(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        let v = err_json(&out);
        assert_eq!(v["code"], "Usage", "{args:?}");
        assert_trace_id(&v);
    }
}

/// The log has the run under the error's `trace_id`, as JSON lines, and nothing from
/// under `$HOME` or the plan's caption text, even with a plan stored under `$HOME`.
#[test]
fn logs_carry_the_trace_id_and_no_home_paths_or_captions() {
    let root = scratch("redaction");
    let home = root.join("home-of-a-user");
    let logs = root.join("logs");
    std::fs::create_dir_all(home.join("Movies")).unwrap();
    // A plan under $HOME whose caption is distinctive, made invalid so the error path
    // runs too.
    let mut plan: Value = serde_json::from_str(
        &std::fs::read_to_string(repo("schemas/examples/h79-parts-1-3.renderplan.json")).unwrap(),
    )
    .unwrap();
    plan["text_events"][2]["runs"][0]["text"] = "zanzibar quokka captions".into();
    plan["parts"][1]["layers"][0]["place"]["h"] = 560.into();
    let file = home.join("Movies/secret-cut.renderplan.json");
    std::fs::write(&file, plan.to_string()).unwrap();

    let home_s = home.to_str().unwrap();
    for keep_messages in ["0", "1"] {
        let out = run(
            &["plan", "validate", file.to_str().unwrap()],
            &logs,
            &[
                ("HOME", home_s),
                ("USERPROFILE", home_s),
                ("GMCLIP_LOG", "trace"),
                ("GMCLIP_LOG_MESSAGES", keep_messages),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let trace_id = assert_trace_id(&err_json(&out));

        let mut text = String::new();
        for e in std::fs::read_dir(&logs).unwrap() {
            text.push_str(&std::fs::read_to_string(e.unwrap().path()).unwrap());
        }
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect();
        let mine: Vec<&Value> = lines.iter().filter(|l| l["trace_id"] == trace_id).collect();
        assert!(mine.len() >= 3, "start, failure, finish: {text}");
        assert!(
            mine.iter()
                .all(|l| l["spans"][0] == "cmd" && l["name"] == "plan.validate")
        );
        assert!(
            mine.iter()
                .any(|l| l["fields"]["pointer"] == "/parts/1/layers/0/place")
        );
        for needle in [home_s, "home-of-a-user", "secret-cut", "zanzibar", "quokka"] {
            assert!(
                !text.contains(needle),
                "{needle:?} leaked into the log: {text}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn logging_off_writes_nothing() {
    let logs = scratch("off").join("logs");
    let out = run(
        &["plan", "validate", "/nonexistent"],
        &logs,
        &[("GMCLIP_LOG", "off")],
    );
    assert_eq!(out.status.code(), Some(4));
    assert!(!logs.exists());
}

#[test]
fn usage_errors_name_the_argument_cut_to_16_bytes() {
    let out = gmclip(&["--a-very-long-unknown-flag-name"]);
    let v = err_json(&out);
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("--a-very-long-un…"), "{reason}");
    assert!(!reason.contains("flag-name"), "{reason}");
}

/// A closed stdout is an `Io` error, not a panic with an empty stderr.
#[cfg(unix)]
#[test]
fn a_closed_stdout_is_an_io_error() {
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_gmclip"))
        .args([
            "plan",
            "validate",
            &repo("schemas/examples/bench-60-m0.renderplan.json"),
        ])
        .env("GMCLIP_LOG", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    // Either the write raced ahead of the close (success) or it failed cleanly.
    match out.status.code() {
        Some(0) => {}
        Some(5) => assert_eq!(err_json(&out)["code"], "Io"),
        other => panic!("{other:?}: {out:?}"),
    }
}
