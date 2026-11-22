//! `cargo xt fixtures`: each negative fixture must make its CI check fail, and fail for
//! the reason it was written for: W0-02's six (fmt, clippy, failing test, denied licence,
//! banned `ffmpeg-next`, layering), a laundered layering edge, and W0-11's unlocked
//! dependency, unaudited crate and new build-script crate.
//!
//! Fixtures live in `xtask/fixtures/<name>/`, each its own throwaway workspace, outside
//! the main workspace (`exclude` in the root `Cargo.toml`). They build into
//! `target/xt-fixtures` so they leave nothing behind.

use std::path::Path;
use std::process::Command;

struct Fixture {
    name: &'static str,
    /// What CI check it proves goes red.
    check: &'static str,
    /// cargo subcommand and arguments; `{manifest}`, `{deny}` and `{store}` (the repo's
    /// `supply-chain/`) are substituted. `["@deps"]` / `["@supply"]`: run that xt task
    /// in-process on the fixture.
    args: &'static [&'static str],
    /// Must appear in the failing command's output.
    expect: &'static str,
    /// A cargo subcommand that must be installed (`cargo <tool> --version`).
    tool: Option<&'static str>,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "ci-fmt",
        check: "cargo fmt --check",
        args: &["fmt", "--manifest-path", "{manifest}", "--check"],
        expect: "Diff in",
        tool: None,
    },
    Fixture {
        name: "ci-clippy",
        check: "cargo clippy -D warnings",
        args: &[
            "clippy",
            "--manifest-path",
            "{manifest}",
            "--locked",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        expect: "needless_return",
        tool: None,
    },
    Fixture {
        name: "ci-test",
        check: "cargo test",
        args: &["test", "--manifest-path", "{manifest}", "--locked"],
        expect: "fixture_test_fails",
        tool: None,
    },
    Fixture {
        name: "ci-deny-licence",
        check: "cargo deny check licenses",
        args: &[
            "deny",
            "--manifest-path",
            "{manifest}",
            "--config",
            "{deny}",
            "check",
            "licenses",
        ],
        expect: "GPL-3.0",
        tool: Some("deny"),
    },
    Fixture {
        name: "ci-deny-ffmpeg",
        check: "cargo deny check bans",
        args: &[
            "deny",
            "--manifest-path",
            "{manifest}",
            "--config",
            "{deny}",
            "check",
            "bans",
        ],
        expect: "ffmpeg-next",
        tool: Some("deny"),
    },
    Fixture {
        name: "ci-vet-unaudited",
        check: "cargo vet",
        args: &[
            "vet",
            "--locked",
            "--manifest-path",
            "{manifest}",
            "--store-path",
            "{store}",
        ],
        expect: "either:1.13.0 missing",
        tool: Some("vet"),
    },
    Fixture {
        name: "deps-core-render",
        check: "cargo xt deps",
        args: &["@deps"],
        expect: "forbidden edge: gmclip-core -> gmclip-render",
        tool: None,
    },
    Fixture {
        name: "deps-laundered",
        check: "cargo xt deps",
        args: &["@deps"],
        expect: "gmclip-core reaches gmclip-render: gmclip-core -> shim -> gmclip-render",
        tool: None,
    },
    Fixture {
        name: "ci-build-rs",
        check: "cargo xt supply",
        args: &["@supply"],
        expect: "anyhow custom-build",
        tool: None,
    },
    Fixture {
        name: "ci-unlocked",
        check: "--locked",
        args: &[
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--manifest-path",
            "{manifest}",
        ],
        expect: "--locked",
        tool: None,
    },
];

pub fn run(root: &Path, require_all: bool) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let target_dir = root.join("target/xt-fixtures");
    let deny = root.join("deny.toml");
    let store = root.join("supply-chain");
    let mut failures = Vec::new();
    for f in FIXTURES {
        let manifest = root.join("xtask/fixtures").join(f.name).join("Cargo.toml");
        if let Some(tool) = f.tool {
            let present = Command::new(&cargo)
                .args([tool, "--version"])
                .output()
                .is_ok_and(|o| o.status.success());
            if !present {
                let msg = format!("{:<18} cargo-{tool} not installed", f.name);
                if require_all {
                    failures.push(msg);
                } else {
                    println!("SKIP {msg}");
                }
                continue;
            }
        }
        let args: Vec<String> = f
            .args
            .iter()
            .map(|a| {
                a.replace("{manifest}", &manifest.to_string_lossy())
                    .replace("{deny}", &deny.to_string_lossy())
                    .replace("{store}", &store.to_string_lossy())
            })
            .collect();
        let in_process = match f.args {
            ["@deps"] => Some(crate::deps::check(&manifest)),
            ["@supply"] => Some(crate::supply::check(root, &manifest)),
            _ => None,
        };
        if let Some(result) = in_process {
            match result {
                Ok(_) => {
                    failures.push(format!("{:<18} `{}` passed; it must fail", f.name, f.check))
                }
                Err(e) if e.contains(f.expect) => {
                    println!("ok   {:<18} `{}` is red", f.name, f.check)
                }
                Err(e) => failures.push(format!(
                    "{:<18} failed, but not with {:?}:\n{e}",
                    f.name, f.expect
                )),
            }
            continue;
        }
        let out = Command::new(&cargo)
            .args(&args)
            .current_dir(root)
            .env("CARGO_TARGET_DIR", &target_dir)
            .output()
            .map_err(|e| format!("{}: cannot run cargo: {e}", f.name))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            failures.push(format!("{:<18} `{}` passed; it must fail", f.name, f.check));
        } else if !text.contains(f.expect) {
            failures.push(format!(
                "{:<18} `{}` failed, but not with {:?}:\n{text}",
                f.name, f.check, f.expect
            ));
        } else {
            println!("ok   {:<18} `{}` is red", f.name, f.check);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("cargo xt fixtures:\n  {}", failures.join("\n  ")))
    }
}
