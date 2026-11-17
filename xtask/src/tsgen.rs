//! `cargo xt gen [--check]` (A2.01): the UI's TypeScript types for every registry
//! command, from the commands' JSON Schemas, and for the app's event payloads, from its
//! event types. xtask links no gmclip crate (`cargo xt deps`), so it runs each generator
//! through its test — core's `tests/ts_gen.rs` (`app/ui/src/gen/commands.ts`) and the
//! app's `tests/ts_events.rs` (`app/ui/src/gen/events.ts`): with `GMCLIP_GEN_WRITE=1`
//! they write the files; `--check` only compares (what CI's `cargo test` does anyway).

use std::path::Path;
use std::process::Command;

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let check = match args {
        [] => false,
        [a] if a == "--check" => true,
        _ => return Err("usage: cargo xt gen [--check]".into()),
    };
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let gens: [(&str, &[&str]); 2] = [
        (
            "app/ui/src/gen/commands.ts",
            &["test", "-q", "-p", "gmclip-core", "--test", "ts_gen"],
        ),
        (
            "app/ui/src/gen/events.ts",
            &[
                "test",
                "-q",
                "--manifest-path",
                "app/src-tauri/Cargo.toml",
                "--test",
                "ts_events",
            ],
        ),
    ];
    for (file, test) in gens {
        let mut cmd = Command::new(&cargo);
        cmd.current_dir(root).args(test);
        if !check {
            cmd.env("GMCLIP_GEN_WRITE", "1");
        }
        let st = cmd
            .status()
            .map_err(|e| format!("cargo test did not start: {e}"))?;
        if !st.success() {
            return Err(if check {
                format!("{file} is out of date: run `cargo xt gen`")
            } else {
                format!("generating {file} failed")
            });
        }
        println!("{} {file}", if check { "up to date:" } else { "wrote" });
    }
    Ok(())
}
