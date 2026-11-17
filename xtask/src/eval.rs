//! `cargo xt eval h79 …`: the h79 eval (SPEC §17.1.1) through `gmclip-eval`.
//!
//! - `cargo xt eval h79 selftest`: the grader self-tests (golden, v3, 8 mutants).
//! - `cargo xt eval h79 fixture`: the fixture's hashes and forbidden-string grep.
//! - `cargo xt eval h79 [--client claude-code|codex] [--runs 5] [--batch D] [--replay D]`:
//!   run (or replay) a batch into `evals/results/h79/<UTC date-time>/` (default) and score it.
//!   Live runs need lane A2; until then the runner refuses and prints the first command.

use std::path::Path;
use std::process::Command;

/// `YYYY-MM-DDTHHMMSSZ` (UTC): one batch directory per invocation, never merged.
fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 86_400;
    format!(
        "{}T{:02}{:02}{:02}Z",
        utc_date(),
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// `YYYY-MM-DD` (UTC) from the system clock (Howard Hinnant's civil-from-days).
fn utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("h79") {
        return Err("usage: cargo xt eval h79 [selftest | fixture | --client C --runs 5 [--batch D] [--replay D]]".into());
    }
    let rest = &args[1..];
    let eval_args: Vec<String> = match rest.first().map(String::as_str) {
        Some("selftest") => vec!["selftest".into(), "h79".into()],
        Some("fixture") => vec!["fixture".into(), "check".into()],
        _ => {
            let mut v = vec!["run".to_owned(), "h79".to_owned()];
            v.extend(rest.iter().cloned());
            if !rest.iter().any(|a| a == "--batch") {
                v.push("--batch".into());
                v.push(
                    root.join("evals/results/h79")
                        .join(utc_stamp())
                        .display()
                        .to_string(),
                );
            }
            v
        }
    };
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(root)
        .args(["run", "--locked", "-q", "-p", "gmclip-eval", "--"])
        .args(&eval_args)
        .status()
        .map_err(|e| format!("cargo run gmclip-eval: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "gmclip-eval {} failed ({status})",
            eval_args.join(" ")
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_shape() {
        let d = super::utc_date();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"), "{d}");
        let s = super::utc_stamp();
        assert_eq!(s.len(), 18, "{s}");
    }
}
