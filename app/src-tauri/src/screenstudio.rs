//! "Make it look finished" for a recording (decision 0046, `screen_studio_apply`): one
//! click gives a cut the Screen Studio look — a gradient plate behind the screen, the
//! screen padded with rounded corners and a soft shadow and the camera as a bottom-right
//! bubble (a built-in layout on every part), the recorded cursor smoothed with ripple
//! clicks, and zoom sections from the input log. A restore point is taken first and
//! returned (`before`), so "Undo look" is one `restore_to` (human edits are never grouped
//! in a run).

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// The plate the look sits on.
const PLATE: &str = "Dusk";
const BUBBLE: &str = "Screen + camera bubble (bottom right)";
const SCREEN_ONLY: &str = "Screen only";

/// Reads stored assets (a recording's input track) from this app's own asset folder for
/// the holder's zoom planner; it decodes no frames.
#[derive(Debug)]
struct Assets<'a>(&'a AppCore);

impl gmclip_core::MediaRender for Assets<'_> {
    fn name(&self) -> &'static str {
        "app-assets"
    }
    fn see(
        &self,
        _: &gmclip_plan::Plan,
        _: &gmclip_core::host::SeeRequest,
    ) -> Result<gmclip_core::host::SeeImage, gmclip_core::host::HostError> {
        Err(gmclip_core::host::HostError::Unsupported("assets only"))
    }
    fn export(
        &self,
        _: &gmclip_plan::Plan,
        _: &std::path::Path,
        _: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<gmclip_core::host::ExportDone, gmclip_core::host::HostError> {
        Err(gmclip_core::host::HostError::Unsupported("assets only"))
    }
    fn verify(
        &self,
        _: &gmclip_plan::Plan,
        _: &std::path::Path,
    ) -> Result<Value, gmclip_core::host::HostError> {
        Err(gmclip_core::host::HostError::Unsupported("assets only"))
    }
    fn asset_bytes(
        &self,
        asset: &gmclip_model::entities::Asset,
    ) -> Result<Vec<u8>, gmclip_core::host::HostError> {
        let p = self
            .0
            .fonts()
            .asset_file_typed(&asset.sha256, &asset.media_type)
            .ok_or(gmclip_core::host::HostError::Unsupported("no assets folder"))?;
        std::fs::read(p).map_err(|e| gmclip_core::host::HostError::Failed(e.kind().to_string()))
    }
}

/// `screen_studio_apply {cut, plate?}`.
pub fn apply(core: &AppCore, project: &str, cut: &str, plate: Option<&str>) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, Framing, Source};
    let p = core.project(project)?;
    let snap = p.handle.snapshot();
    let doc = snap.doc.clone();
    let cid = crate::planner::ulid_id::<Cut>(cut)?;
    let framing = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::invalid("the cut has no framing yet"))?;
    let fid = crate::planner::ulid_id::<Framing>(&framing)?;
    let parts: Vec<String> = gmclip_model::clock::cut_layout(&doc, cid)
        .iter()
        .map(|l| l.part.0.to_string())
        .collect();
    if parts.is_empty() {
        return Err(AppError::invalid("the cut has no parts"));
    }
    let recorded: Vec<String> = doc
        .iter::<Source>()
        .filter(|s| s.cursor.is_some())
        .map(|s| s.id.0.to_string())
        .collect();
    let groups = doc.iter::<gmclip_model::entities::SyncGroup>().count();

    let before = core.call(project, "restore_point_create", json!({}))?;
    let before = before["output"]["id"].as_str().unwrap_or_default().to_owned();
    let mut notes = Vec::new();
    let r = (|| -> AppResult<()> {
        core.call(
            project,
            "style_library_apply",
            json!({"kind": "plate", "name": plate.unwrap_or(PLATE), "to": {"framing": framing}}),
        )?;
        // the bubble needs a camera: a recording's face file pairs as a sync group
        let layout = if groups > 0 { BUBBLE } else { SCREEN_ONLY };
        if let Err(e) = core.call(
            project,
            "layout_builtin_add",
            json!({"cut": cut, "framing": framing, "name": layout, "parts": parts}),
        ) {
            notes.push(format!("layout: {}", e.message));
        }
        for s in &recorded {
            core.call(
                project,
                "source_cursor_set",
                json!({"source": s, "smoothness": 0.6, "click": "ripple", "click_shrink": true}),
            )?;
        }
        if recorded.is_empty() {
            notes.push("no recorded cursor: no smoothing or auto zoom".into());
        } else {
            let snap = p.handle.snapshot();
            match gmclip_mcp::cursor::zoom_auto_plan(&Assets(core), &snap, cid, Some(fid), None) {
                Ok(z) if !z.sections.is_empty() => {
                    core.call(project, "section_auto_apply", z.apply_args(cid))?;
                    notes.push(format!("{} zoom sections", z.sections.len()));
                }
                Ok(z) => notes.push(format!(
                    "nothing in the log to zoom on ({} parts read, {} skipped)",
                    z.parts_read, z.parts_skipped
                )),
                Err(e) => notes.push(format!("auto zoom: {e}")),
            }
        }
        Ok(())
    })();
    r?;
    Ok(json!({"applied": true, "before": before, "notes": notes}))
}

/// The camera as a bottom-right bubble over the screen on every part of `cut`, as a
/// recording with a face file opens (otherwise only the screen shows). One undo step.
pub fn camera_bubble(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    use gmclip_model::entities::Cut;
    let snap = core.project(project)?.handle.snapshot();
    let cid = crate::planner::ulid_id::<Cut>(cut)?;
    let framing = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::invalid("the cut has no framing yet"))?;
    let parts: Vec<String> = gmclip_model::clock::cut_layout(&snap.doc, cid)
        .iter()
        .map(|l| l.part.0.to_string())
        .collect();
    core.call(
        project,
        "layout_builtin_add",
        json!({"cut": cut, "framing": framing, "name": BUBBLE, "parts": parts}),
    )
}

/// `zoom_auto_app {cut, max_zoom?}`: the lane's Auto zoom (decision 0046). The holder's
/// planner reads the input log (`zoom_auto` is holder-answered, so the app runs the same
/// code here) and `section_auto_apply` replaces the cut's earlier automatic zooms in the
/// active framing; hand-made ones stay. One undo step.
pub fn zoom_auto(
    core: &AppCore,
    project: &str,
    cut: &str,
    max_zoom: Option<f64>,
) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, Framing};
    let p = core.project(project)?;
    let cid = crate::planner::ulid_id::<Cut>(cut)?;
    let fid = core
        .active_framing(project, cut)
        .and_then(|f| crate::planner::ulid_id::<Framing>(&f).ok());
    let snap = p.handle.snapshot();
    let z = gmclip_mcp::cursor::zoom_auto_plan(&Assets(core), &snap, cid, fid, max_zoom)?;
    if z.parts_read == 0 {
        return Err(AppError::invalid(
            "no part's screen has an input log (recorded with gmclip): add zoom sections by hand",
        ));
    }
    core.call(project, "section_auto_apply", z.apply_args(cid))?;
    Ok(json!({
        "sections": z.sections.len(), "parts_read": z.parts_read, "parts_skipped": z.parts_skipped,
    }))
}
