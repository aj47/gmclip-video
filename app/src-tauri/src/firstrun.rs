//! First run for a stranger (SPEC §12.5, adapted to AJ's direction of 2026-10-15; lane S)
//! and the first-run checklist on Home.
//!
//! **Setup** asks four questions, each with a picture in the UI:
//! 1. *What do you record?* → a kit (§10.5): Gameplay + facecam, Screen + face, Talking
//!    head / podcast, Something else (Blank).
//! 2. *Where do you post?* → framings, safe zones, export presets and loudness (−14 LUFS).
//! 3. *Find moments with:* your own agent CLI (the clients the launcher detects, with
//!    their versions; one gmclip cannot hold to its tools is shown and refused) on its own
//!    subscription, or "I'll pick moments myself". AI runs only through your agent: no API
//!    keys, no local highlights.
//! 4. *Where are your recordings?* → the recordings folder (agents may read it: an Owner
//!    grant) and where the file picker starts.
//!
//! The answers become one summary line per default ([`summary`]). Apply makes a profile
//! from the kit with the posting answers as its content (gmclip never rewrites a profile:
//! changed answers make a new one), then raises **one** native sheet listing every Owner
//! change (use the profile, the recordings folder, the default agent) and runs them. Every
//! line is editable later on the one Settings page, which reopens this sheet.
//!
//! **Checklist**: after the first real import Home shows five steps — layout → pick a
//! moment → trim by words → check captions → export — each ticked from what the projects
//! actually hold, dismissible. State in `<app data>/setup.json` (UI state, not authority).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult};

/// `(id, label, framing, export preset, safe zones)`.
pub const PLATFORMS: &[(&str, &str, &str, &str, &str)] = &[
    ("tiktok", "TikTok", "9:16", "reels-1080x1920", "tiktok"),
    (
        "shorts",
        "YouTube Shorts",
        "9:16",
        "reels-1080x1920",
        "shorts",
    ),
    (
        "reels",
        "Instagram Reels",
        "9:16",
        "reels-1080x1920",
        "instagram",
    ),
    (
        "youtube",
        "YouTube (16:9)",
        "16:9",
        "youtube-1080p",
        "youtube",
    ),
];

/// What the setup asks for a kit, in the order the sheet shows it.
pub const KIT_CHOICES: &[(&str, &str, &str)] = &[
    (
        "gaming-variety",
        "Gameplay + facecam",
        "Reactions, clutches and fails; camera on top, game below; 8–45 s.",
    ),
    (
        "screen-face",
        "Screen + face",
        "Coding, tutorials, builds; the screen panel with your face; 20–75 s.",
    ),
    (
        "talking-head",
        "Talking head / podcast",
        "One or more people talking; centred on the speaker; 20–90 s.",
    ),
    (
        "blank",
        "Something else",
        "Start blank and set things up as you go.",
    ),
];

/// The setup's answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Answers {
    pub kit: String,
    pub platforms: Vec<String>,
    /// `agent` (with `harness`) or `self`.
    pub finder: String,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub recordings: Option<String>,
}

/// `<app data>/setup.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SetupState {
    #[serde(default)]
    pub done_ms: Option<i64>,
    #[serde(default)]
    pub answers: Option<Answers>,
    /// The profile the last apply made or reused.
    #[serde(default)]
    pub profile: Option<PathBuf>,
    /// The checklist was dismissed.
    #[serde(default)]
    pub checklist_dismissed: bool,
    /// "Don't transcribe for now" (the speech model download was declined).
    #[serde(default)]
    pub no_transcribe: bool,
}

fn state_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("setup.json")
}

pub fn load(core: &AppCore) -> SetupState {
    std::fs::read(state_path(core))
        .ok()
        .filter(|b| b.len() < 1 << 20)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(core: &AppCore, st: &SetupState) -> AppResult<()> {
    std::fs::create_dir_all(&core.dirs.app_data)
        .map_err(|e| AppError::internal(format!("app data ({})", e.kind())))?;
    let b = serde_json::to_vec_pretty(st).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&state_path(core), &b)
        .map_err(|e| AppError::internal(format!("saving the setup ({})", e.kind())))
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The recordings folder the setup suggests: `~/Movies` (macOS's own place for video;
/// Windows: `%USERPROFILE%\Videos`).
fn default_recordings() -> Option<PathBuf> {
    let name = if cfg!(windows) { "Videos" } else { "Movies" };
    home_dir().map(|h| h.join(name))
}

/// Whether setup is done (Home's welcome card; no agent detection, no model hashing).
pub fn state(core: &AppCore) -> Value {
    let st = load(core);
    json!({"done": st.done_ms.is_some(), "answers": st.answers, "no_transcribe": st.no_transcribe})
}

/// What the sheet needs: the choices, the agents found (with versions), the current
/// answers, the speech model's state.
pub fn preview(core: &AppCore) -> Value {
    let st = load(core);
    let kits: Vec<Value> = KIT_CHOICES
        .iter()
        .map(|(id, label, blurb)| json!({"id": id, "label": label, "blurb": blurb}))
        .collect();
    let platforms: Vec<Value> = PLATFORMS
        .iter()
        .map(|(id, label, framing, preset, zone)| {
            json!({"id": id, "label": label, "framing": framing, "preset": preset, "zone": zone})
        })
        .collect();
    json!({
        "kits": kits,
        "platforms": platforms,
        "agents": crate::agent::clients_json(core, false),
        "recordings_default": default_recordings(),
        "answers": st.answers,
        "done": st.done_ms.is_some(),
        "no_transcribe": st.no_transcribe,
        "models": crate::models::status(),
    })
}

fn check(a: &Answers) -> AppResult<()> {
    if !KIT_CHOICES.iter().any(|k| k.0 == a.kit) {
        return Err(AppError::invalid("pick what you record"));
    }
    if a.platforms.is_empty() {
        return Err(AppError::invalid("pick at least one place you post"));
    }
    if let Some(p) = a
        .platforms
        .iter()
        .find(|p| !PLATFORMS.iter().any(|x| x.0 == p.as_str()))
    {
        return Err(AppError::invalid(format!(
            "“{}” is not a place gmclip exports for",
            p.chars().take(40).collect::<String>()
        )));
    }
    match a.finder.as_str() {
        "self" => {}
        "agent" if a.harness.is_some() => {}
        "agent" => return Err(AppError::invalid("pick which of your agents finds moments")),
        _ => {
            return Err(AppError::invalid(
                "find moments with your agent or yourself",
            ));
        }
    }
    if let Some(r) = &a.recordings
        && (!std::path::Path::new(r).is_absolute()
            || r.len() > 1024
            || r.chars().any(char::is_control))
    {
        return Err(AppError::invalid(
            "the recordings folder is an absolute path",
        ));
    }
    Ok(())
}

/// The profile content the posting answers add on top of the kit's.
pub fn platform_content(platforms: &[String]) -> BTreeMap<String, Value> {
    let chosen: Vec<&(&str, &str, &str, &str, &str)> = PLATFORMS
        .iter()
        .filter(|p| platforms.iter().any(|x| x == p.0))
        .collect();
    let mut framings: Vec<&str> = chosen.iter().map(|p| p.2).collect();
    framings.sort_unstable_by_key(|f| if *f == "9:16" { 0 } else { 1 });
    framings.dedup();
    let mut presets: Vec<&str> = chosen.iter().map(|p| p.3).collect();
    presets.dedup();
    let zones: Vec<&str> = chosen.iter().map(|p| p.4).collect();
    let mut c = BTreeMap::new();
    c.insert(
        "platforms".into(),
        json!(chosen.iter().map(|p| p.0).collect::<Vec<_>>()),
    );
    c.insert("default_framings".into(), json!(framings));
    c.insert("export_presets".into(), json!(presets));
    c.insert("safe_zones".into(), json!(zones));
    // every platform normalises to about −14 LUFS (SPEC §12.5)
    c.insert(
        "loudness".into(),
        json!({"mode": "static", "integrated_lufs": -14.0, "true_peak_dbtp": -1.0}),
    );
    c
}

/// One line per default the answers set: `[{id, label, value}]`.
pub fn summary(core: &AppCore, a: &Answers) -> AppResult<Vec<Value>> {
    check(a)?;
    let kit = KIT_CHOICES
        .iter()
        .find(|k| k.0 == a.kit)
        .map_or("", |k| k.1);
    let (kname, kmd, _) =
        gmclip_core::profile::kits::kit(&a.kit).ok_or_else(|| AppError::invalid("no such kit"))?;
    let kc = gmclip_core::profile::kits::content_of(kmd).map_err(|e| AppError::internal(e.0))?;
    let pc = platform_content(&a.platforms);
    let labels: Vec<&str> = PLATFORMS
        .iter()
        .filter(|p| a.platforms.iter().any(|x| x == p.0))
        .map(|p| p.1)
        .collect();
    let band = kc
        .get("duration_bands")
        .and_then(|b| b.get("9:16"))
        .map(|b| format!("{}–{} s", b[0], b[1]));
    let template = match kc.get("default_template").and_then(Value::as_str) {
        Some("gameplay_facecam_split") => "camera across the top, game below, captions in the seam",
        Some("screen_face") => "screen panel on top, your face below",
        Some("talking_head") => "centred on the speaker",
        _ => "the whole picture, centred",
    };
    let mut lines = vec![
        json!({"id": "kit", "label": "What you record", "value": format!("{kit} — profile “{kname}”")}),
        json!({"id": "layout", "label": "New vertical cuts", "value": template}),
        json!({"id": "captions", "label": "Captions", "value": kc.get("caption_preset").and_then(Value::as_str).map_or("the default style".to_owned(), |p| format!("the “{p}” style"))}),
    ];
    if let Some(b) = band {
        lines.push(json!({"id": "band", "label": "Moment length (9:16)", "value": b}));
    }
    lines.push(json!({"id": "platforms", "label": "Where you post", "value": labels.join(", ")}));
    lines.push(json!({"id": "framings", "label": "Shapes", "value": pc["default_framings"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" and ")).unwrap_or_default()}));
    lines.push(json!({"id": "zones", "label": "Safe zones", "value": format!("{} (kept clear of the apps' buttons; TikTok's and Shorts' are measured later, Instagram's are used until then)", pc["safe_zones"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default())}));
    lines.push(json!({"id": "loudness", "label": "Loudness", "value": "−14 LUFS, −1 dBTP peak"}));
    lines.push(match (a.finder.as_str(), &a.harness) {
        ("agent", Some(h)) => {
            let line = crate::agent::default_effect(core, h).map_or_else(|e| e.message, |x| x.1);
            json!({"id": "finder", "label": "Finding moments", "value": format!("your agent — {line}")})
        }
        _ => json!({"id": "finder", "label": "Finding moments", "value": "you pick them (no agent runs)"}),
    });
    lines.push(json!({"id": "media", "label": "Agent sees and hears footage", "value": "off — asked per run"}));
    let rec = a
        .recordings
        .clone()
        .or_else(|| default_recordings().map(|p| p.to_string_lossy().into_owned()));
    if let Some(r) = rec {
        // the same test Apply uses: only a folder that exists is granted
        let value = if Path::new(&r).is_dir() {
            format!("{r} (agents may read it, never write)")
        } else {
            format!("{r} — no folder there, so none is set (pick one later in Settings)")
        };
        lines.push(json!({"id": "recordings", "label": "Recordings folder", "value": value}));
    }
    Ok(lines)
}

/// The profile for these answers: the one made last time when the kit and places are
/// unchanged, else a new folder (gmclip never rewrites a profile).
fn profile_for(core: &AppCore, st: &SetupState, a: &Answers) -> AppResult<(PathBuf, bool)> {
    if let (Some(prev), Some(p)) = (&st.answers, &st.profile)
        && prev.kit == a.kit
        && prev.platforms == a.platforms
        && p.join(gmclip_core::profile::PROFILE_FILE).is_file()
    {
        return Ok((p.clone(), false));
    }
    let base = core
        .dirs
        .library
        .parent()
        .unwrap_or(&core.dirs.library)
        .join("Profiles");
    let name = gmclip_core::profile::kits::kit(&a.kit)
        .map(|k| k.0)
        .unwrap_or("Mine");
    let stem = crate::core::safe_name(name);
    let mut dir = base.join(&stem);
    let mut n = 2;
    while dir.exists() {
        dir = base.join(format!("{stem} {n}"));
        n += 1;
    }
    Ok((dir, true))
}

/// Applies the answers: the profile (content), then **one** native sheet listing every
/// Owner change, then each change. `dry_run` returns the summary and the sheet's lines
/// without doing anything.
pub fn apply(core: &AppCore, a: &Answers, dry_run: bool) -> AppResult<Value> {
    let lines = summary(core, a)?;
    let mut st = load(core);
    let (dir, fresh) = profile_for(core, &st, a)?;
    let rec = a
        .recordings
        .clone()
        .or_else(|| default_recordings().map(|p| p.to_string_lossy().into_owned()))
        .filter(|r| Path::new(r).is_dir());
    let mut owner: Vec<(String, Value)> =
        vec![("profile_use".into(), json!({"path": dir.to_string_lossy()}))];
    let mut sheet: Vec<String> = Vec::new();
    sheet.extend(crate::owner::effects("profile_use", &owner[0].1)?.1);
    if let Some(r) = &rec {
        let args = json!({"path": r});
        sheet.extend(crate::owner::effects("recordings_set", &args)?.1);
        owner.push(("recordings_set".into(), args));
    }
    let harness = match (a.finder.as_str(), &a.harness) {
        ("agent", Some(h)) => {
            let (h, line) = crate::agent::default_effect(core, h)?;
            sheet.push(line);
            Some(h)
        }
        _ => None,
    };
    if dry_run {
        return Ok(json!({"summary": lines, "sheet": sheet, "profile": dir, "new_profile": fresh}));
    }
    let text = sheet
        .iter()
        .map(|l| format!("• {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    if !crate::sheet::confirm("Set up gmclip?", &text, "Set up") {
        return Err(AppError::new(
            "Unauthorized",
            "not allowed (the Owner did not approve it on the sheet)",
        ));
    }
    if fresh {
        let name = KIT_CHOICES
            .iter()
            .find(|k| k.0 == a.kit)
            .map_or("Mine", |k| k.1);
        gmclip_core::profile::kits::create_from_kit_with(
            &dir,
            &a.kit,
            name,
            &platform_content(&a.platforms),
        )
        .map_err(|e| AppError::invalid(e.0))?;
        gmclip_core::profile::register_local(&core.dirs.app_data, &dir)
            .map_err(|e| AppError::internal(e.0))?;
    }
    let mut done = Vec::new();
    for (cmd, args) in &owner {
        core.agents.owner_call(cmd, args)?;
        done.push(cmd.clone());
    }
    if let Some(h) = harness {
        crate::agent::save_default(core, h)?;
        done.push("default_agent".into());
    }
    st.done_ms = Some(now_ms());
    st.answers = Some(a.clone());
    st.profile = Some(dir.clone());
    save(core, &st)?;
    Ok(json!({"applied": done, "profile": dir, "new_profile": fresh, "summary": lines}))
}

/// "Don't transcribe for now" / undo it.
pub fn set_no_transcribe(core: &AppCore, on: bool) -> AppResult<Value> {
    let mut st = load(core);
    st.no_transcribe = on;
    save(core, &st)?;
    Ok(json!({"no_transcribe": on}))
}

// ---- the first-run checklist ------------------------------------------------------------

/// The five steps, each ticked from what the open projects hold.
pub fn checklist(core: &AppCore) -> Value {
    let st = load(core);
    let projects = core.projects();
    // only after the first real import: an open project with a source
    let imported = projects.iter().any(|p| {
        p.handle
            .snapshot()
            .doc
            .iter::<gmclip_model::entities::Source>()
            .next()
            .is_some()
    });
    let mut layout = false;
    let mut moment = false;
    let mut trimmed = false;
    let mut captions = false;
    let mut exported = false;
    for p in &projects {
        let doc = p.handle.snapshot().doc.clone();
        use gmclip_model::entities::{CaptionDoc, Cut, Framing, Layout, Moment, MomentState, Part};
        layout |= doc.iter::<Layout>().next().is_some()
            || doc.iter::<Framing>().any(|f| f.canvas.h > f.canvas.w);
        moment |= doc.iter::<Moment>().any(|m| m.state == MomentState::Staged)
            || doc.iter::<Cut>().any(|c| c.moment.is_some());
        // trimmed by words: a strike or a split leaves a cut with more than one part
        trimmed |= doc.iter::<Part>().count() > doc.iter::<Cut>().count();
        captions |= doc.iter::<CaptionDoc>().any(|c| c.enabled);
        exported |= !p.exports.lock().map(|e| e.is_empty()).unwrap_or(true);
    }
    let steps = [
        // (the id stays "layout": the step is done by a vertical framing or a drawn layout)
        (
            "layout",
            "Make it vertical",
            "Switch the cut to 9:16 at the top of the cut.",
            layout,
        ),
        (
            "moment",
            "Pick a moment",
            "Find the bit worth a short: search what you said (⌘⇧F) and press ＋, or ask your agent.",
            moment,
        ),
        (
            "trim",
            "Trim by words",
            "Strike words or split a part where it drags (the words under the picture, or Words).",
            trimmed,
        ),
        (
            "captions",
            "Check captions",
            "Turn captions on and read them through (Captions).",
            captions,
        ),
        ("export", "Export", "Export the cut (⌘E).", exported),
    ];
    let list: Vec<Value> = steps
        .iter()
        .map(|(id, label, how, done)| json!({"id": id, "label": label, "how": how, "done": done}))
        .collect();
    let all = steps.iter().all(|s| s.3);
    json!({"show": imported && !st.checklist_dismissed && !all, "dismissed": st.checklist_dismissed,
           "imported": imported, "steps": list, "done": all})
}

pub fn dismiss_checklist(core: &AppCore, dismissed: bool) -> AppResult<Value> {
    let mut st = load(core);
    st.checklist_dismissed = dismissed;
    save(core, &st)?;
    Ok(checklist(core))
}
