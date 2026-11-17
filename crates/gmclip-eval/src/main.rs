//! `gmclip-eval`: see the crate docs. Subcommands:
//! - `mcp-stub <record>`: the stand-in MCP server (stdio).
//! - `s7 --harness claude-code|codex --client <abs path> --work <dir> [--out <report.json>]`
//! - `stub-model --turns <turns.json> --url-out <file> --log <requests.jsonl>`: the
//!   scripted loopback model on its own (for lab tests in crates that may not depend on
//!   this one); `turns.json` is `[[[tool, input], …], …]`; it runs until stdin closes.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use gmclip_ai::launcher::Harness;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result: Result<(), String> = match args.first().map(String::as_str) {
        Some("mcp-stub") => match args.get(1) {
            Some(p) => {
                let only: Option<Vec<String>> =
                    arg(&args, "--tools").map(|t| t.split(',').map(str::to_owned).collect());
                gmclip_eval::mcp_stub::run(
                    &PathBuf::from(p),
                    only.as_deref(),
                    arg(&args, "--responses").map(PathBuf::from).as_deref(),
                )
                .map_err(|e| e.to_string())
            }
            None => Err("usage: gmclip-eval mcp-stub <record>".into()),
        },
        Some("s7") => s7(&args[1..]),
        Some("stub-model") => stub_model(&args[1..]),
        Some("s7-control") => s7_control(&args[1..]),
        Some("grade") if args.get(1).map(String::as_str) == Some("h79") => grade(&args[2..]),
        Some("selftest") if args.get(1).map(String::as_str) == Some("h79") => selftest(),
        Some("score") if args.get(1).map(String::as_str) == Some("h79") => score(&args[2..]),
        Some("run") if args.get(1).map(String::as_str) == Some("h79") => run(&args[2..]),
        Some("injection") => injection(&args[1..]),
        Some("fixture") if args.get(1).map(String::as_str) == Some("check") => fixture_check(),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn stub_model(args: &[String]) -> Result<(), String> {
    use gmclip_eval::stub_model::{Script, StubModel};
    let need = |n: &str| arg(args, n).ok_or(format!("stub-model needs {n}"));
    let turns: serde_json::Value =
        serde_json::from_slice(&std::fs::read(need("--turns")?).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut script = Vec::new();
    for turn in turns.as_array().ok_or("turns: an array")? {
        let mut calls = Vec::new();
        for c in turn.as_array().ok_or("a turn: an array")? {
            let name = c[0].as_str().ok_or("a call: [tool, input]")?;
            calls.push((name.to_owned(), c[1].clone()));
        }
        script.push(calls);
    }
    let model = StubModel::start_with(Script::AnthropicTurns(script)).map_err(|e| e.to_string())?;
    // written whole (temp + rename): a reader never sees a partial URL
    let url_out = PathBuf::from(need("--url-out")?);
    let tmp = url_out.with_extension("tmp");
    std::fs::write(&tmp, &model.base_url).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &url_out).map_err(|e| e.to_string())?;
    // until the caller closes stdin; then the requests, one JSON body per line
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    let log: String = model
        .take()
        .iter()
        .map(|r| format!("{}\n", serde_json::json!({"path": r.path, "body": r.body})))
        .collect();
    std::fs::write(need("--log")?, log).map_err(|e| e.to_string())
}

fn h79() -> PathBuf {
    gmclip_eval::h79::bundle::h79_dir()
}

fn grade(args: &[String]) -> Result<(), String> {
    use gmclip_eval::h79::{bundle, grade};
    let dir = args
        .first()
        .ok_or("usage: gmclip-eval grade h79 <bundle dir> [--reference]")?;
    let mode = if args.iter().any(|a| a == "--reference") {
        bundle::Mode::Reference
    } else {
        bundle::Mode::Run
    };
    let b = bundle::Bundle::load(&PathBuf::from(dir), mode)?;
    let r = grade::Reference::load(&h79())?;
    let rep = grade::grade(&b, &r)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
    );
    if rep.verdict == grade::Verdict::Fail {
        return Err(format!("fail: {:?}", rep.failed));
    }
    Ok(())
}

fn selftest() -> Result<(), String> {
    use gmclip_eval::h79::{grade, selftest};
    let r = grade::Reference::load(&h79())?;
    let rows = selftest::run_all(&r)?;
    let mut ok = true;
    for row in &rows {
        ok &= row.ok;
        println!(
            "{} {:<52} expect {:<34} got {:?} failed {:?} pending {:?}",
            if row.ok { "ok  " } else { "FAIL" },
            row.name,
            row.expect,
            row.verdict,
            row.failed,
            row.pending
        );
    }
    if ok {
        Ok(())
    } else {
        Err("grader self-test failed".into())
    }
}

fn score(args: &[String]) -> Result<(), String> {
    use gmclip_eval::h79::score;
    let dir = args
        .first()
        .ok_or("usage: gmclip-eval score h79 <batch dir>")?;
    let reg: score::Preregistration = serde_json::from_slice(
        &std::fs::read(h79().join("preregistration.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let s = score::score(&PathBuf::from(dir), &reg)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    use gmclip_eval::h79::run;
    let clients: Vec<String> = match arg(args, "--client") {
        Some(c) => vec![c],
        None => vec!["claude-code".into(), "codex".into()],
    };
    let runs: usize = arg(args, "--runs")
        .unwrap_or_else(|| "5".into())
        .parse()
        .map_err(|_| "--runs N")?;
    let batch = arg(args, "--batch")
        .map(PathBuf::from)
        .ok_or("--batch <dir> (evals/results/h79/<date>)")?;
    let driver = match arg(args, "--replay") {
        Some(d) => run::Driver::Replay(PathBuf::from(d)),
        None => run::Driver::Live,
    };
    let s = run::run(&h79(), &clients, runs, &batch, &driver)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn injection(args: &[String]) -> Result<(), String> {
    use gmclip_eval::injection;
    let dir = injection::cases_dir();
    let cases = injection::load_cases(&dir)?;
    let case = |a: &[String]| -> Result<injection::Case, String> {
        let id: u32 = arg(a, "--case")
            .and_then(|c| c.parse().ok())
            .ok_or("--case N")?;
        cases
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| format!("no case {id}"))
    };
    let print = |v: &dyn erased::Ser| println!("{}", v.json());
    match args.first().map(String::as_str) {
        Some("plan") => {
            let r = injection::plan_report(&cases, args.iter().any(|a| a == "--media"))?;
            print(&r);
            Ok(())
        }
        Some("project") => {
            let c = case(args)?;
            let out = PathBuf::from(arg(args, "--out").ok_or("--out <dir>")?);
            injection::make_project(&c, &dir, &out)?;
            println!("case {} project: {}", c.id, out.display());
            Ok(())
        }
        Some("score") => {
            let c = case(args)?;
            let run = PathBuf::from(arg(args, "--run").ok_or("--run <run.json>")?);
            let root = arg(args, "--fixture-root").ok_or("--fixture-root <abs project dir>")?;
            let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .unwrap_or_default();
            let row = injection::score_run(&c, &run, &root, &home)?;
            print(&row);
            Ok(())
        }
        Some("smoke") => {
            let client = PathBuf::from(arg(args, "--client").ok_or("--client <abs claude>")?);
            let work = PathBuf::from(arg(args, "--work").ok_or("--work <dir>")?);
            let me = std::env::current_exe().and_then(std::fs::canonicalize).map_err(|e| e.to_string())?;
            let s = injection::smoke(&client, &work, &me, &dir)?;
            print(&s);
            Ok(())
        }
        _ => Err("usage: gmclip-eval injection <plan [--media] | project --case N --out D | score --case N --run R --fixture-root D | smoke --client P --work D>".into()),
    }
}

mod erased {
    /// Pretty JSON of anything serialisable, behind a trait object.
    pub trait Ser {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize> Ser for T {
        fn json(&self) -> String {
            serde_json::to_string_pretty(self).unwrap_or_default()
        }
    }
}

fn fixture_check() -> Result<(), String> {
    use gmclip_eval::h79::{bundle, fixture};
    let truth = bundle::load_truth(&h79())?;
    let f = fixture::check(&h79(), &truth.forbidden_fixture_strings)?;
    if f.is_empty() {
        println!("fixture ok: hashes match the manifest; no forbidden string; h79's cut excluded");
        Ok(())
    } else {
        Err(f.join("\n"))
    }
}

const USAGE: &str = "usage: gmclip-eval <mcp-stub <record> [--tools a,b] | s7|s7-control --harness H --client P --work D [--out F] \
| stub-model --turns F --url-out F --log F | grade h79 <bundle> [--reference] | selftest h79 | score h79 <batch> | run h79 [--client C] [--runs 5] --batch D [--replay D] | fixture check>";

fn s7_control(args: &[String]) -> Result<(), String> {
    let h = arg(args, "--harness")
        .and_then(|h| Harness::parse(&h))
        .ok_or(USAGE)?;
    let client = PathBuf::from(arg(args, "--client").ok_or(USAGE)?);
    let work = PathBuf::from(arg(args, "--work").ok_or(USAGE)?);
    let me = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let leaked = gmclip_eval::s7::control(h, &client, &work, &me)?;
    println!(
        "control ({}, no isolation): canaries that reached the model: {leaked:?}",
        h.as_str()
    );
    Ok(())
}

fn s7(args: &[String]) -> Result<(), String> {
    let h = arg(args, "--harness")
        .and_then(|h| Harness::parse(&h))
        .ok_or(USAGE)?;
    let client = PathBuf::from(arg(args, "--client").ok_or(USAGE)?);
    let work = PathBuf::from(arg(args, "--work").ok_or(USAGE)?);
    let me = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let report = gmclip_eval::s7::probe(h, &client, &work, &me)?;
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    if let Some(out) = arg(args, "--out") {
        std::fs::write(&out, &json).map_err(|e| e.to_string())?;
    }
    for c in &report.checks {
        println!(
            "{} {} {} — {}",
            if c.pass { "PASS" } else { "FAIL" },
            c.id,
            c.what,
            c.detail
        );
    }
    println!(
        "{} {}: {}",
        report.harness.as_str(),
        report.version,
        if report.supported {
            "SUPPORTED"
        } else {
            "UNSUPPORTED"
        }
    );
    Ok(())
}
