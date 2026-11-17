//! Words the transcriber gets wrong (Ideas 2026-10-16, K.02; SPEC §5.4 terms, §5.10 term
//! gate): one term list — the project's (`term_set`, `term_fix`), the active profile's
//! `profile.json` terms (read only) and the terms saved for the profile in the app data
//! ("in all projects"; the profile folder is content gmclip never writes) — read by
//! three things:
//!
//! - the **correction pass** when a recording is transcribed and when a cut's own audio
//!   is heard (`gmclip_core::terms::correct_words`): listed variants spelt as the term
//!   before the words are stored, a correction over the words, never a model change;
//! - the **term gate** before export (`short::term_gate`);
//! - **Fix everywhere** from a caption word: `term_fix` (every caption in the project
//!   shows the term, the variant joins the project's list, one undo) and, when asked, the
//!   profile's list too.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gmclip_model::Doc;
use gmclip_model::entities::{Cut, Term};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

fn store_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("profile-terms.json")
}

type Store = BTreeMap<String, Vec<Term>>;

fn load_store(core: &AppCore) -> Store {
    std::fs::read(store_path(core))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The terms saved for the active profile in the app ("in all projects").
pub fn saved_terms(core: &AppCore) -> Vec<Term> {
    load_store(core)
        .remove(&crate::takelayout::profile_key(core))
        .unwrap_or_default()
}

/// Adds `heard` as a variant of `written` in the active profile's saved terms.
pub fn save_profile_term(core: &AppCore, written: &str, heard: &str) -> AppResult<()> {
    let key = crate::takelayout::profile_key(core);
    let mut st = load_store(core);
    let list = st.entry(key).or_default();
    let k = written.to_lowercase();
    let i = match list.iter().position(|t| t.written.to_lowercase() == k) {
        Some(i) => i,
        None => {
            if list.len() >= 500 {
                return Err(AppError::invalid("the profile has 500 terms already"));
            }
            list.push(Term {
                written: written.to_owned(),
                heard_as: Vec::new(),
            });
            list.len() - 1
        }
    };
    let t = &mut list[i];
    t.written = written.to_owned();
    let hk = gmclip_core::terms::key(heard);
    if hk != gmclip_core::terms::key(written)
        && !t.heard_as.iter().any(|h| gmclip_core::terms::key(h) == hk)
        && t.heard_as.len() < 50
    {
        t.heard_as.push(heard.to_owned());
    }
    let bytes = serde_json::to_vec_pretty(&st).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&store_path(core), &bytes)
        .map_err(|e| AppError::internal(format!("saving the profile's terms: {}", e.kind())))
}

/// The one term list: the project's, then the profile's `profile.json` terms, then the
/// terms saved for the profile here — a later spelling of the same term only adds its
/// variants.
pub fn term_list(core: &AppCore, doc: &Doc) -> Vec<Term> {
    let mut out = gmclip_core::terms::project_terms(doc);
    let mut add = |t: Term| match out
        .iter_mut()
        .find(|x| x.written.to_lowercase() == t.written.to_lowercase())
    {
        Some(x) => {
            for h in t.heard_as {
                let hk = gmclip_core::terms::key(&h);
                if !x.heard_as.iter().any(|y| gmclip_core::terms::key(y) == hk) {
                    x.heard_as.push(h);
                }
            }
        }
        None => out.push(t),
    };
    let profile = gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data).profile;
    if let Some(p) = profile.and_then(|d| gmclip_core::profile::load(&d).ok())
        && let Some(list) = p.content.get("terms").and_then(Value::as_array)
    {
        for t in list {
            if let Ok(t) = serde_json::from_value::<Term>(t.clone()) {
                add(t);
            }
        }
    }
    for t in saved_terms(core) {
        add(t);
    }
    out
}

/// Fix everywhere from caption words: the cut's transcript words `from..=to` (1–4 of
/// them, as heard) become `written` in every caption of the project (`term_fix`), and
/// with `profile` in every project from now on.
#[allow(clippy::too_many_arguments)]
pub fn fix_everywhere(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    written: &str,
    profile: bool,
    owner: Option<&str>,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    // the words' own video's transcript (a cut may play several, decision 0036)
    let (_, _, t, _) = crate::words::transcript_for(core, &p, &doc, cid, owner)
        .ok_or_else(|| AppError::new("Unsupported", "no transcript for this cut yet"))?;
    if to < from || to - from >= gmclip_core::terms::MAX_RUN as u64 {
        return Err(AppError::invalid("pick 1 to 4 words"));
    }
    let heard = (from..=to)
        .map(|i| {
            t.words.get(i as usize).map(|w| {
                w.text
                    .trim_end_matches([',', '.', '?', '!', ':', ';'])
                    .to_owned()
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| AppError::invalid("no such word"))?
        .join(" ");
    let written = written.trim();
    let r = core.call(
        project,
        "term_fix",
        json!({"heard": heard, "written": written}),
    )?;
    if profile {
        save_profile_term(core, written, &heard)?;
    }
    let mut out = r["output"].clone();
    out["heard"] = json!(heard);
    out["written"] = json!(written);
    out["profile"] = json!(profile);
    Ok(out)
}

/// `term_fix_everywhere`, `terms_view`.
pub fn ipc(core: &AppCore, command: &str, project: &str, a: &Value) -> AppResult<Value> {
    match command {
        "term_fix_everywhere" => {
            let n = |k: &str| {
                a.get(k)
                    .and_then(Value::as_u64)
                    .ok_or_else(|| AppError::invalid(format!("`{k}` is a word")))
            };
            fix_everywhere(
                core,
                project,
                a.get("cut")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AppError::invalid("`cut` is required"))?,
                n("from")?,
                n("to")?,
                a.get("written").and_then(Value::as_str).unwrap_or(""),
                a.get("profile").and_then(Value::as_bool).unwrap_or(false),
                a.get("owner").and_then(Value::as_str),
            )
        }
        "terms_view" => {
            let p = core.project(project)?;
            let doc = p.handle.snapshot().doc.clone();
            let project_terms = gmclip_core::terms::project_terms(&doc);
            let saved = saved_terms(core);
            Ok(json!({
                "terms": term_list(core, &doc).iter().map(|t| json!({
                    "written": t.written, "heard_as": t.heard_as,
                    "project": project_terms.iter().any(|x| x.written.eq_ignore_ascii_case(&t.written)),
                    "profile": saved.iter().any(|x| x.written.eq_ignore_ascii_case(&t.written)),
                })).collect::<Vec<_>>(),
            }))
        }
        _ => Err(AppError::invalid("unknown command")),
    }
}
