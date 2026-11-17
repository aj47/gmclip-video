//! `gmclip profile …` (SPEC §10.5): kits make content-only profiles, a profile's
//! suggested settings are shown and not applied, and the CLI has no way to append style
//! feedback (R-PROF-2).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn scratch(name: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("gmclip-cli-prof-{name}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

/// Never the real home: app data (the local-profile list) goes under a temp HOME.
fn gmclip(args: &[&str]) -> Output {
    let home = std::env::temp_dir().join(format!("gmclip-cli-prof-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_gmclip"))
        .args(args)
        .env("GMCLIP_LOG", "off")
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        // Windows app data is %LOCALAPPDATA%\gmclip
        .env("USERPROFILE", &home)
        .env("LOCALAPPDATA", home.join("AppData").join("Local"))
        .output()
        .unwrap()
}

fn ok(args: &[&str]) -> Value {
    let o = gmclip(args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout).unwrap()
}

fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            v.extend(tree(&p));
        } else {
            v.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    v.sort();
    v
}

#[test]
fn kits_make_content_only_profiles() {
    let d = scratch("kit");
    let kits = ok(&["profile", "kits"]);
    assert_eq!(kits["kits"].as_array().unwrap().len(), 4);
    let p = d.join("mine");
    let v = ok(&[
        "profile",
        "new",
        p.to_str().unwrap(),
        "--kit",
        "gaming-variety",
    ]);
    assert_eq!(v["kit"], "gaming-variety");
    assert_eq!(v["origin"], "local", "made here");
    // a copy of the folder elsewhere is someone else's, whatever profile.json says
    let copy = d.join("copy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(p.join("profile.json"), copy.join("profile.json")).unwrap();
    assert_eq!(
        ok(&["profile", "show", copy.to_str().unwrap()])["origin"],
        "external"
    );
    assert!(
        v["content_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "trigger_phrases")
    );
    assert_eq!(v["suggested_settings_not_applied"][0]["key"], "style_path");
    assert!(p.join("style.md").exists());
    let again = gmclip(&["profile", "new", p.to_str().unwrap(), "--kit", "blank"]);
    assert!(!again.status.success(), "never over an existing profile");
    // no CLI command appends style feedback (R-PROF-2): `feedback` only exports
    for args in [
        &["feedback", "append", "x"][..],
        &["feedback", "add", "x"][..],
        &["profile", "feedback", "x"][..],
    ] {
        assert!(!gmclip(args).status.success(), "{args:?}");
    }
    let help = gmclip(&["feedback", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("export"), "{help}");
    let subcommands: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(subcommands, ["export", "help"], "{help}");
    let _ = std::fs::remove_dir_all(&d);
}

/// `gmclip feedback export --format agents-md` prints the log as AGENTS.md lines that read
/// back as the same records, and writes nothing.
#[test]
fn feedback_exports_as_agents_md_lines() {
    let d = scratch("fbx");
    gmclip_core::profile::feedback::append_by_human(
        &d,
        "2026-10-14",
        "keep the clock",
        None,
        "AJ",
        "app",
    )
    .unwrap();
    gmclip_core::profile::feedback::append_by_human(
        &d,
        "2026-10-15",
        "less jumpy\n## not a heading",
        Some("01J0000000000000000000000A"),
        "AJ",
        "review-link:abc",
    )
    .unwrap();
    let before = tree(&d);
    let o = gmclip(&[
        "feedback",
        "export",
        "--format",
        "agents-md",
        "--profile",
        d.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let md = String::from_utf8(o.stdout).unwrap();
    let back = gmclip_core::profile::feedback::parse_agents_md(&md);
    assert_eq!(back.len(), 2, "{md}");
    assert_eq!(back[0].0, "2026-10-14");
    assert!(
        back[0].1.starts_with("keep the clock (AJ, via app)"),
        "{md}"
    );
    assert!(
        back[1].1.contains("via review-link:abc, export 01J"),
        "{md}"
    );
    assert_eq!(tree(&d), before, "export writes nothing");
    let o = gmclip(&[
        "feedback",
        "export",
        "--format",
        "json",
        "--profile",
        d.to_str().unwrap(),
    ]);
    assert!(!o.status.success(), "agents-md only");
    let _ = std::fs::remove_dir_all(&d);
}
