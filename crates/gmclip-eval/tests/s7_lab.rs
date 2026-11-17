//! S7 on the installed clients (lab; zero spend: a loopback stub model refuses every
//! request). Run with the clients' absolute paths:
//!
//! ```text
//! GMCLIP_S7_CLAUDE=/abs/claude GMCLIP_S7_CODEX=/abs/codex \
//!   cargo test -p gmclip-eval --test s7_lab -- --ignored --nocapture
//! ```
//!
//! The verdict for each client must equal its `s7` line in `evals/h79/clients.toml`; a
//! new client version that changes the verdict fails here until the file (and the
//! launcher's range table) are updated from a fresh probe.

use std::path::PathBuf;

use gmclip_ai::launcher::Harness;

fn probe(h: Harness, var: &str) {
    let Some(client) = std::env::var_os(var) else {
        eprintln!("{var} not set; skipping {}", h.as_str());
        return;
    };
    let work = std::env::temp_dir().join(format!(
        "gmclip-s7-lab-{}-{}",
        h.as_str(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    let report = gmclip_eval::s7::probe(
        h,
        &PathBuf::from(client),
        &work,
        PathBuf::from(env!("CARGO_BIN_EXE_gmclip-eval")).as_path(),
    )
    .unwrap();
    for c in &report.checks {
        eprintln!(
            "{} {} {}",
            if c.pass { "PASS" } else { "FAIL" },
            c.id,
            c.detail
        );
    }
    let clients = gmclip_eval::clients::parse(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../evals/h79/clients.toml"
        ))
        .unwrap(),
    )
    .unwrap();
    let expect_pass = clients[h.as_str()]["s7"] == "pass";
    assert!(
        report.checks.iter().find(|c| c.id == "S7.0").unwrap().pass,
        "the probe never reached the stub"
    );
    assert_eq!(
        report.supported,
        expect_pass,
        "{} {}",
        h.as_str(),
        report.version
    );
    // sensitivity: without the launcher's flags the same home leaks
    let control = gmclip_eval::s7::control(
        h,
        &PathBuf::from(std::env::var_os(var).unwrap()),
        &work.join("control"),
        PathBuf::from(env!("CARGO_BIN_EXE_gmclip-eval")).as_path(),
    )
    .unwrap();
    eprintln!("control (no isolation) leaked {control:?}");
    assert!(
        !control.is_empty(),
        "the control run leaked nothing: the probe cannot see a leak"
    );
}

#[test]
#[ignore = "lab: needs installed clients"]
fn s7_claude_code() {
    probe(Harness::ClaudeCode, "GMCLIP_S7_CLAUDE");
}

#[test]
#[ignore = "lab: needs installed clients"]
fn s7_codex() {
    probe(Harness::Codex, "GMCLIP_S7_CODEX");
}
