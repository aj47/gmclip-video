//! End card to the long-form (lane K5): a short whose moment comes from a stream that has
//! a long-form gets, on one click from the Short tab, "Full video:" with the long-form's
//! title, a frame of it and an arrow over its last 3 s (`end_card_longform_add`: the
//! brand kit's face and colours, overlay kinds that exist). It is a suggestion: never
//! added by itself, one ⌘Z (or Remove) takes it off.
//!
//! - `end_card_longform_view{cut}`: whether a long-form plays this short's moment (the
//!   one sharing the most of its source time), the title the card would show (an
//!   accepted YouTube title draft of the long-form, else its own title), and whether the
//!   short has the card already;
//! - `end_card_longform_add_app{cut}`: the card, with the long-form's frame at the
//!   short's moment (no picture when the recording's file is gone); a framing of another
//!   shape made later lays it out again (`framing::ensure` → `end_card_longform_fit`);
//! - `end_card_longform_remove{cut}`: the card off, one undo step.

use gmclip_core::commands::brand::LONGFORM_CARD_BAND;
use gmclip_model::entities::{
    CopyDraft, CopyPlatform, Cut, CutKind, MediaRef, Overlay, OverlayParent, Part, ProposalState,
};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

/// The overlays of the card on `cut`.
fn card_overlays(doc: &gmclip_model::Doc, cut: gmclip_model::Id<Cut>) -> Vec<String> {
    doc.children::<Overlay>(cut.entity())
        .filter_map(|id| doc.get(id))
        .filter(|o| {
            o.parent == OverlayParent::Cut(cut) && o.band.as_deref() == Some(LONGFORM_CARD_BAND)
        })
        .map(|o| o.id.0.to_string())
        .collect()
}

/// What the card would point at: `(long-form, title shown, its own title, media, source
/// time of the moment in flicks)`, or why there is none.
type Target = (gmclip_model::Id<Cut>, String, String, MediaRef, i64);

fn target(doc: &gmclip_model::Doc, cid: gmclip_model::Id<Cut>) -> Result<Target, &'static str> {
    let c = doc.get(cid).ok_or("no such cut")?;
    if !c.kind.is_short() {
        return Err("End cards to the long-form go on shorts");
    }
    let spans: Vec<(MediaRef, i64, i64)> = gmclip_model::clock::cut_layout(doc, cid)
        .iter()
        .filter_map(|sp| doc.get::<Part>(sp.part))
        .map(|p| (p.media, p.src_in.0, p.src_out.0))
        .collect();
    if spans.is_empty() {
        return Err("This short has no parts yet");
    }
    let mut best: Option<(i64, gmclip_model::Id<Cut>, MediaRef, i64)> = None;
    for long in doc.iter::<Cut>().filter(|x| x.kind == CutKind::Long) {
        let mut shared = 0i64;
        let mut first: Option<(MediaRef, i64)> = None;
        for lp in doc
            .children::<Part>(long.id.entity())
            .filter_map(|id| doc.get(id))
        {
            for (m, a, b) in &spans {
                if *m != lp.media {
                    continue;
                }
                let (x, y) = ((*a).max(lp.src_in.0), (*b).min(lp.src_out.0));
                if x < y {
                    shared += y - x;
                    if first.is_none_or(|f| x < f.1) {
                        first = Some((*m, x));
                    }
                }
            }
        }
        if let Some((m, at)) = first
            && best.as_ref().is_none_or(|b| shared > b.0)
        {
            best = Some((shared, long.id, m, at));
        }
    }
    let (_, long, media, at) = best.ok_or(
        "No long-form plays this moment yet: make one from the stream's topics (project map → Long-forms)",
    )?;
    let own = doc.get(long).map(|l| l.title.clone()).unwrap_or_default();
    // an accepted YouTube title draft of the long-form is what it is published as
    let shown = doc
        .children::<CopyDraft>(long.entity())
        .filter_map(|id| doc.get(id))
        .filter(|d| d.platform == CopyPlatform::Youtube && d.state == ProposalState::Accepted)
        .max_by_key(|d| d.at_ms)
        .map(|d| d.title.trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| own.clone());
    Ok((long, shown, own, media, at))
}

/// Whether `cut` has the end card to the long-form.
pub fn has_card(core: &AppCore, project: &str, cut: &str) -> bool {
    let (Ok(p), Ok(cid)) = (core.project(project), ulid_id::<Cut>(cut)) else {
        return false;
    };
    !card_overlays(&p.handle.snapshot().doc, cid).is_empty()
}

/// `end_card_longform_view{cut}`.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let has = !card_overlays(&doc, cid).is_empty();
    Ok(match target(&doc, cid) {
        Ok((long, title, own, _, at)) => json!({
            "available": true, "longform": long.0.to_string(), "title": title,
            "longform_title": own, "at_s": at as f64 / FLICKS as f64, "has_card": has}),
        Err(why) => json!({"available": false, "why": why, "has_card": has}),
    })
}

/// `end_card_longform_add_app{cut}`.
pub fn add(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let (long, title, _, media, at) =
        target(&doc, cid).map_err(|why| AppError::new("Unsupported", why))?;
    // the long-form's frame at the short's moment (the card goes on without one when
    // the recording's file moved)
    let image = crate::broll::picture_path(&doc, &media)
        .and_then(|(path, fps)| crate::broll::frame_png(&path, at as f64 / FLICKS as f64, fps).ok())
        .and_then(|png| crate::studio::store_image_file(core, &png).ok())
        .map(|(sha, size, w, h)| {
            json!({"name": format!("{title} (frame)").chars().take(200).collect::<String>(),
                   "sha256": sha, "size": size, "w": w, "h": h})
        });
    let mut args = json!({"cut": cut, "longform": long.0.to_string(), "title": title});
    if let Some(im) = &image {
        args["image"] = im.clone();
    }
    let r = core.call(project, "end_card_longform_add", args)?;
    Ok(json!({"ids": r["output"]["ids"], "title": title, "picture": image.is_some()}))
}

/// `end_card_longform_remove{cut}`: one undo step.
pub fn remove(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let ids = card_overlays(&doc, ulid_id::<Cut>(cut)?);
    if ids.is_empty() {
        return Err(AppError::not_found(
            "this short has no end card to the long-form",
        ));
    }
    let ops: Vec<Value> = ids
        .iter()
        .map(|o| json!({"op": "overlay_delete", "overlay": o}))
        .collect();
    core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))?;
    Ok(json!({"removed": ids.len()}))
}
