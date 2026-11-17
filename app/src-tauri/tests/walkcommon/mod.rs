//! Shared by the walkthrough drivers: ffmpeg measurements (a dev tool, comparison
//! only) and the comparison of an export with RT's MP4 and ASS.
#![allow(dead_code)]

use std::path::Path;

use serde_json::{Value, json};

pub const FF: &str = "/opt/homebrew/bin/ffmpeg";

pub fn ff(args: &[&str]) -> String {
    let o = std::process::Command::new(FF).args(args).output().unwrap();
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn loudness(f: &str) -> (f64, f64) {
    let t = ff(&[
        "-hide_banner",
        "-nostats",
        "-i",
        f,
        "-af",
        "ebur128=peak=true",
        "-f",
        "null",
        "-",
    ]);
    let tail = &t[t.rfind("Summary:").unwrap_or(0)..];
    let num = |key: &str| -> f64 {
        tail.lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|x| x.parse().ok())
            .unwrap_or(f64::NAN)
    };
    (num("I:"), num("Peak:"))
}

/// Mean SSIM and the share of frames under 0.8, ours against RT's (scaled alike).
pub fn ssim(a: &str, b: &str, stats: &Path) -> (f64, f64, usize) {
    let filter = format!("[0:v][1:v]ssim=stats_file={}", stats.display());
    let t = ff(&[
        "-hide_banner",
        "-nostats",
        "-i",
        a,
        "-i",
        b,
        "-lavfi",
        &filter,
        "-f",
        "null",
        "-",
    ]);
    let all = t
        .lines()
        .find_map(|l| l.split("All:").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|x| x.parse().ok())
        .unwrap_or(f64::NAN);
    let per: Vec<f64> = std::fs::read_to_string(stats)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split("All:")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .collect();
    let low = per.iter().filter(|x| **x < 0.8).count();
    (all, low as f64 / per.len().max(1) as f64, per.len())
}

pub fn ass_time(s: &str) -> f64 {
    let p: Vec<f64> = s.split(':').filter_map(|x| x.parse().ok()).collect();
    p.first().copied().unwrap_or(0.0) * 3600.0
        + p.get(1).copied().unwrap_or(0.0) * 60.0
        + p.get(2).copied().unwrap_or(0.0)
}

pub fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace("\\N", " ")
}

pub fn words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Compares our export (`pl`, `ours`) with RT's MP4 and ASS: loudness, SSIM, caption words
/// and cue boundaries, the other text events; writes side-by-side frames under
/// `$WALK_FRAMES/<tag>-<t>s.png`. Fills `report`; returns the one-line note.
#[allow(clippy::too_many_arguments)]
pub fn compare(
    pl: &gmclip_plan::Plan,
    ours: &str,
    theirs: &str,
    ass_path: &Path,
    root: &Path,
    tag: &str,
    frame_times: &[&str],
    report: &mut serde_json::Map<String, Value>,
) -> String {
    let caps: Vec<&gmclip_plan::types::TextEvent> = pl
        .text_events
        .iter()
        .flatten()
        .filter(|t| t.role == gmclip_plan::types::TextRole::Caption)
        .collect();
    let (li, lp) = loudness(ours);
    let (ri, rp) = loudness(theirs);
    let (sm, low, n) = ssim(ours, theirs, &root.join("ssim.log"));
    report.insert("loudness".into(), json!({"ours": [li, lp], "rt": [ri, rp]}));
    report.insert(
        "ssim".into(),
        json!({"mean": sm, "share_below_0_8": low, "frames": n}),
    );
    // captions: RT's Speech cues vs ours
    let ass = std::fs::read_to_string(ass_path).unwrap();
    let rt_caps: Vec<(f64, f64, String)> = ass
        .lines()
        .filter(|l| l.starts_with("Dialogue:") && l.contains(",Speech,"))
        .map(|l| {
            let f: Vec<&str> = l.splitn(10, ',').collect();
            (ass_time(f[1]), ass_time(f[2]), strip_tags(f[9]))
        })
        .collect();
    let our_caps: Vec<(f64, f64, String)> = caps
        .iter()
        .map(|t| {
            let text: String = t
                .runs
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>()
                .join("");
            (t.out.start() as f64 / 30.0, t.out.end() as f64 / 30.0, text)
        })
        .collect();
    let rw: Vec<String> = rt_caps.iter().flat_map(|c| words(&c.2)).collect();
    let ow: Vec<String> = our_caps.iter().flat_map(|c| words(&c.2)).collect();
    // word-level LCS: how much of RT's caption text ours carries, in order
    let mut dp = vec![vec![0usize; ow.len() + 1]; rw.len() + 1];
    for i in 0..rw.len() {
        for j in 0..ow.len() {
            dp[i + 1][j + 1] = if rw[i] == ow[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    let lcs = dp[rw.len()][ow.len()];
    // cue boundaries: over the words both carry (the LCS pairs), does a word that starts a
    // cue in RT start one in ours, and the reverse
    let firsts = |caps: &[(f64, f64, String)]| -> Vec<bool> {
        caps.iter()
            .flat_map(|c| {
                let n = words(&c.2).len();
                (0..n).map(|i| i == 0).collect::<Vec<_>>()
            })
            .collect()
    };
    let (rf, of) = (firsts(&rt_caps), firsts(&our_caps));
    let mut pairs = Vec::new();
    let (mut i, mut j) = (rw.len(), ow.len());
    while i > 0 && j > 0 {
        if rw[i - 1] == ow[j - 1] {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    let rt_starts = pairs.iter().filter(|(a, _)| rf[*a]).count();
    let both = pairs.iter().filter(|(a, b)| rf[*a] && of[*b]).count();
    let ours_only = pairs.iter().filter(|(a, b)| !rf[*a] && of[*b]).count();
    // whole cues with the same words
    let rt_set: std::collections::BTreeSet<Vec<String>> =
        rt_caps.iter().map(|c| words(&c.2)).collect();
    let same_cues = our_caps
        .iter()
        .filter(|c| rt_set.contains(&words(&c.2)))
        .count();
    // cue start timing: each RT cue's first word against the nearest of ours starting with it
    let mut deltas = Vec::new();
    for (s, _, t) in &rt_caps {
        if let Some(w) = words(t).first()
            && let Some(d) = our_caps
                .iter()
                .filter(|c| words(&c.2).first() == Some(w))
                .map(|c| (c.0 - s).abs())
                .min_by(|a, b| a.partial_cmp(b).unwrap())
        {
            deltas.push(d);
        }
    }
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = deltas.get(deltas.len() / 2).copied().unwrap_or(f64::NAN);
    report.insert("captions".into(), json!({
        "story_cues": rt_caps.len(), "our_cues": our_caps.len(),
        "rt_words": rw.len(), "our_words": ow.len(), "words_in_common_in_order": lcs,
        "boundaries": {"rt_cue_starts_on_shared_words": rt_starts, "matched": both, "ours_not_rt": ours_only,
                       "identical_cues": same_cues},
        "start_delta_median_s": med, "start_delta_p90_s": deltas.get(deltas.len() * 9 / 10).copied(),
        "ours": our_caps.iter().map(|c| json!([c.0, c.1, c.2])).collect::<Vec<_>>(),
        "rt": rt_caps.iter().map(|c| json!([c.0, c.1, c.2])).collect::<Vec<_>>(),
    }));
    let others: Vec<Value> = pl
        .text_events
        .iter()
        .flatten()
        .filter(|t| t.role != gmclip_plan::types::TextRole::Caption)
        .map(|t| json!({"role": format!("{:?}", t.role), "from_s": t.out.start() as f64 / 30.0, "to_s": t.out.end() as f64 / 30.0,
                        "text": t.runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("")}))
        .collect();
    let rt_other: Vec<Value> = ass
        .lines()
        .filter(|l| l.starts_with("Dialogue:") && !l.contains(",Speech,"))
        .map(|l| {
            let f: Vec<&str> = l.splitn(10, ',').collect();
            json!({"style": f[3], "from_s": ass_time(f[1]), "to_s": ass_time(f[2]), "text": strip_tags(f[9])})
        })
        .collect();
    report.insert("overlays".into(), json!({"ours": others, "rt": rt_other}));
    // side by side at a few moments: ours left, RT right
    if let Some(dir) = std::env::var_os("WALK_FRAMES") {
        for t in frame_times {
            let png = Path::new(&dir).join(format!("{tag}-{t}s.png"));
            ff(&[
                "-hide_banner",
                "-y",
                "-ss",
                t,
                "-i",
                ours,
                "-ss",
                t,
                "-i",
                theirs,
                "-frames:v",
                "1",
                "-filter_complex",
                "[0:v]scale=360:640[a];[1:v]scale=360:640[b];[a][b]hstack",
                &png.to_string_lossy(),
            ]);
        }
    }
    format!(
        "SSIM {sm:.3} ({:.0}% frames < 0.8); loudness {li:.1}/{lp:.1} vs RT {ri:.1}/{rp:.1}; captions {lcs}/{} RT words in order, cue starts {both}/{rt_starts} matched (+{ours_only} of ours), {same_cues} identical cues",
        low * 100.0,
        rw.len()
    )
}
