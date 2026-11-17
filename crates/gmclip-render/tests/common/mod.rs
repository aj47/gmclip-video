//! Lab inputs: the golden-11 cuts in the lab media folder (read-only), their frozen
//! RenderPlans (`lab/fixtures/golden-11`) and the local Neometric Heavy font.
#![allow(dead_code)]

use std::path::PathBuf;

/// The 10 story reels with `plan.json` + `qa/heard.json` (R-CAP-1).
pub const REELS: [&str; 10] = [
    "loc_086da141_h03",
    "loc_086da141_h25",
    "loc_086da141_h47",
    "loc_4fadb756_h34",
    "loc_4fadb756_h37",
    "loc_4fadb756_h48",
    "loc_bd89f38b_h41",
    "loc_bd89f38b_h43",
    "loc_bd89f38b_h53",
    "loc_bd89f38b_h88",
];

/// A golden reel's folder in the lab media folder.
pub fn reel_dir(hook: &str) -> PathBuf {
    gmclip_model::lab::media("reels").join(hook)
}

/// The 11 golden cuts' names: the 10 reels and h79 v4.
pub fn golden_names() -> Vec<String> {
    let mut v: Vec<String> = REELS.iter().map(|h| h.to_string()).collect();
    v.push("loc_4fadb756_h79".into());
    v
}

/// A golden cut's frozen RenderPlan (`lab/fixtures/golden-11`) with its lab paths
/// resolved.
pub fn golden_plan(name: &str) -> gmclip_plan::Plan {
    let p = gmclip_model::lab::fixture(format!("golden-11/{name}.renderplan.json"));
    let mut plan = gmclip_plan::parse(&std::fs::read(&p).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    plan
}

/// A golden cut's reference render in the lab media folder.
pub fn golden_mp4(name: &str) -> PathBuf {
    if name.ends_with("h79") {
        gmclip_model::lab::media("h79/h79-v4.mp4")
    } else {
        reel_dir(name).join(format!("{name}-v5.mp4"))
    }
}

/// The 11 golden ASS files: 10 reels' `captions-v5.ass` and h79's `captions-v4.ass`.
pub fn golden_ass() -> Vec<(String, PathBuf)> {
    let mut v: Vec<_> = REELS
        .iter()
        .map(|h| (h.to_string(), reel_dir(h).join("captions-v5.ass")))
        .collect();
    v.push((
        "loc_4fadb756_h79".into(),
        gmclip_model::lab::media("h79/captions-v4.ass"),
    ));
    v
}

/// `GMCLIP_NEOMETRIC`, default the lab media folder's `fonts/Neometric Heavy (Bold).otf`.
pub fn neometric() -> PathBuf {
    std::env::var_os("GMCLIP_NEOMETRIC")
        .map(PathBuf::from)
        .unwrap_or_else(|| gmclip_model::lab::media("fonts/Neometric Heavy (Bold).otf"))
}

/// A golden cut's ASS file (`captions-v5.ass`, h79's `captions-v4.ass`).
pub fn golden_ass_of(name: &str) -> PathBuf {
    if name.ends_with("h79") {
        gmclip_model::lab::media("h79/captions-v4.ass")
    } else {
        reel_dir(name).join("captions-v5.ass")
    }
}

/// The golden cuts whose frozen plan's sources, and the files `extra(name)` names, are on
/// this machine. A lab that measures the 11 runs on [`gmclip_model::lab::Present::items`]
/// and says which subset in its output ([`gmclip_model::lab::Present::note`]).
pub fn goldens_here(extra: impl Fn(&str) -> Vec<PathBuf>) -> gmclip_model::lab::Present<String> {
    gmclip_model::lab::present(golden_names(), String::clone, |n| {
        let mut v: Vec<PathBuf> = golden_plan(n)
            .sources
            .iter()
            .map(|s| PathBuf::from(&s.path))
            .collect();
        v.extend(extra(n));
        v
    })
}
