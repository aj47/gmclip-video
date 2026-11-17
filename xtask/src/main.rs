//! `cargo xt <task>` (alias in `.cargo/config.toml`).
//!
//! - `deps [--manifest-path <Cargo.toml>]`: enforce the crate dependency direction
//!   (SPEC §3.1, implementer Part C) and the `unsafe` policy, from `cargo metadata`.
//! - `check-registry`: every `#[command]` type is listed in the core registry, every listed
//!   type is a `#[command]`, names match `^[a-z0-9_]{1,64}$` and are unique, and the list
//!   is sorted by name (SPEC §3.2); parsed with syn.
//! - `supply [--manifest-path <Cargo.toml>]`: no build-script or proc-macro crate new
//!   to the tree (SR R-SUP-1), against `supply-chain/build-and-proc-macro.txt`.
//! - `codeowners [--check] [paths…]`: who must approve a path under `.github/CODEOWNERS`;
//!   `--check` requires the protected paths (codec/, supply chain, CI) to be AJ's (A-04).
//! - `bench60 [--sources P] [--out P] [--no-validate]`: write the BENCH-60-M0 RenderPlan
//!   (SPEC §13, H-15) and validate it with `gmclip plan validate`.
//! - `stylegate build|serve|score …`: the R-COMP-2 blind A/B page (H-13; see stylegate.rs).
//! - `fixtures [--require-all]`: run each negative CI fixture and require it to fail
//!   for the right reason (W0-02, W0-11). `--require-all` turns a missing tool
//!   (cargo-deny) into an error instead of a skip; CI passes it.
//! - `corpus <verify|print> [--skip-missing] [--class C] [--id ID]`: check
//!   `test-media/manifest.toml` against the bytes on this machine (W0-05).
//! - `stream-sources`: check `test-media/stream-sources.toml` (W0-12).
//! - `eval h79 [selftest | fixture | --client C --runs 5 …]`: the h79 agent eval
//!   (SPEC §17.1.1) through `gmclip-eval` (see eval.rs).
//! - `fuzz-replay`: every committed FZ14/FZ15 input (`fuzz/{seeds,regressions}/`)
//!   through the fuzzer's harness bodies, on stable (U.19; see fuzzreplay.rs).
//! - `gen [--check]`: the UI's TypeScript types for every registry command, from their
//!   JSON Schemas (`app/ui/src/gen/commands.ts`), and for the app's event payloads
//!   (`app/ui/src/gen/events.ts`), A2.01; see tsgen.rs.
#![forbid(unsafe_code)]

mod bench60;
mod codeowners;
mod corpus;
mod deps;
mod eval;
mod fixtures;
mod fuzzreplay;
mod minitoml;
mod nodevbridge;
mod registry;
mod streamsources;
mod stylegate;
mod supply;
mod tsgen;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xt <deps [--manifest-path P] | supply [--manifest-path P] | \
check-registry | codeowners [--check] [paths] | stylegate … | bench60 [--out P] | fixtures [--require-all] | \
corpus <verify|print> [..] | stream-sources | eval h79 … | assert-no-dev-bridge <binary|.app> | fuzz-replay | gen [--check]>";

/// The repository root (xtask lives one level below it).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some(task @ ("deps" | "supply")) => {
            let manifest = match args.get(1).map(String::as_str) {
                None => repo_root().join("Cargo.toml"),
                Some("--manifest-path") => match args.get(2) {
                    Some(p) => PathBuf::from(p),
                    None => return usage(),
                },
                Some(_) => return usage(),
            };
            if task == "deps" {
                deps::check(&manifest)
            } else {
                supply::check(&repo_root(), &manifest)
            }
            .map(|report| println!("{report}"))
        }
        Some("check-registry") => registry::check(&repo_root()).map(|report| println!("{report}")),
        Some("bench60") => bench60::run(&repo_root(), &args[1..]),
        Some("stylegate") => stylegate::run(&args[1..]),
        Some("codeowners") => codeowners::run(&repo_root(), &args[1..]),
        Some("fixtures") => {
            let require_all = args.iter().any(|a| a == "--require-all");
            fixtures::run(&repo_root(), require_all)
        }
        Some("corpus") => corpus::run(&repo_root(), &args[1..]),
        Some("eval") => eval::run(&repo_root(), &args[1..]),
        Some("assert-no-dev-bridge") => nodevbridge::run(&args[1..]),
        Some("fuzz-replay") => fuzzreplay::run(&repo_root()),
        Some("stream-sources") => {
            streamsources::run(&repo_root()).map(|report| println!("{report}"))
        }
        Some("gen") => tsgen::run(&repo_root(), &args[1..]),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
