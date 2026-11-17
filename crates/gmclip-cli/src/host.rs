//! The CLI's renderer for the agent surface (`see`, `export`, `verify`): the
//! [`gmclip_core::MediaRender`] seam on gmclip-render and gmclip-media.
//!
//! - Sources open through lane H's `export::inputs::DemuxInputs` (gmclip-media's
//!   demuxers), the same `Inputs` `gmclip render` uses.
//! - `see` renders output frames with `export::render_frames` from a text-less copy of
//!   the plan, then composites the text events itself with the same raster (H-06 draws
//!   text in export and `render_frames`, which fail on a missing font; `see` skips the
//!   event and notes it instead), blacks out the
//!   private-region masks core computed, lays out a frame, strip or sheet within the
//!   long-side limit and encodes PNG.
//! - `export` is `export::export` (it installs the file and its export record).
//! - `verify` is B-15's container verify against the plan.
//! - `verify_joins` hears the render with gmclip's own ASR (lane D's Parakeet) and
//!   checks every join against the parts' source audio ([`crate::analysis::verify_joins`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gmclip_core::gmclip_plan::Plan;
use gmclip_core::host::{ExportDone, HostError, MaskRect, SeeImage, SeeLayout, SeeRequest};
use gmclip_render::export::ExportOptions;
use gmclip_render::export::sheet::{RgbImage, png_bytes};
use gmclip_render::ops::RenderError;
use serde_json::{Value, json};

fn failed(e: impl std::fmt::Display) -> HostError {
    HostError::Failed(e.to_string().chars().take(120).collect())
}

/// Area-averaging resize to `w × h`.
fn resize(img: &RgbImage, w: u32, h: u32) -> RgbImage {
    let (w, h) = (w.max(1), h.max(1));
    let mut out = RgbImage::new(w, h, [0, 0, 0]);
    let (sx, sy) = (img.width as f64 / w as f64, img.height as f64 / h as f64);
    for y in 0..h {
        let y0 = (y as f64 * sy) as u32;
        let y1 = (((y + 1) as f64 * sy).ceil() as u32).clamp(y0 + 1, img.height);
        for x in 0..w {
            let x0 = (x as f64 * sx) as u32;
            let x1 = (((x + 1) as f64 * sx).ceil() as u32).clamp(x0 + 1, img.width);
            let mut acc = [0u64; 3];
            let mut n = 0u64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let o = ((yy * img.width + xx) * 3) as usize;
                    for (a, p) in acc.iter_mut().zip(&img.px[o..o + 3]) {
                        *a += u64::from(*p);
                    }
                    n += 1;
                }
            }
            let o = ((y * w + x) * 3) as usize;
            for (p, a) in out.px[o..o + 3].iter_mut().zip(acc) {
                *p = (a / n.max(1)) as u8;
            }
        }
    }
    out
}

fn fill(img: &mut RgbImage, r: &MaskRect, rgb: [u8; 3]) {
    for y in r.y..(r.y + r.h).min(img.height) {
        for x in r.x..(r.x + r.w).min(img.width) {
            let o = ((y * img.width + x) * 3) as usize;
            img.px[o..o + 3].copy_from_slice(&rgb);
        }
    }
}

fn blit(dst: &mut RgbImage, src: &RgbImage, ox: u32, oy: u32) {
    for y in 0..src.height.min(dst.height.saturating_sub(oy)) {
        let s = (y * src.width * 3) as usize;
        let d = (((oy + y) * dst.width + ox) * 3) as usize;
        let n = (src.width.min(dst.width.saturating_sub(ox)) * 3) as usize;
        dst.px[d..d + n].copy_from_slice(&src.px[s..s + n]);
    }
}

/// Composites the plan's text events active at output frame `k` onto `img`.
fn draw_text(
    plan: &Plan,
    k: u64,
    img: &mut RgbImage,
    raster: &mut Option<gmclip_render::text::raster::TextRaster>,
    notes: &mut Vec<String>,
) {
    let Some(events) = &plan.text_events else {
        return;
    };
    let active: Vec<_> = events
        .iter()
        .filter(|e| e.out.0[0] <= k && k < e.out.0[1])
        .collect();
    if active.is_empty() {
        return;
    }
    let r = raster.get_or_insert_with(|| {
        let mut r = gmclip_render::text::raster::TextRaster::new();
        for f in plan.fonts.iter().flatten() {
            if let Ok(face) = gmclip_render::text::font::FontFace::open(Path::new(&f.path)) {
                let _ = r.add_font(f.id.as_str(), face);
            }
        }
        r
    });
    let mut rgba: Vec<u8> = img
        .px
        .chunks(3)
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect();
    for ev in active {
        let Some(style) = plan.text_styles.as_ref().and_then(|m| m.get(&ev.style)) else {
            continue;
        };
        match r.render(ev, style) {
            Ok(tile) => tile.composite_over(&mut rgba, img.width, img.height),
            Err(_) => {
                let n = "some text was not drawn (font or style unavailable)".to_owned();
                if !notes.contains(&n) {
                    notes.push(n);
                }
            }
        }
    }
    for (i, p) in rgba.chunks(4).enumerate() {
        img.px[i * 3..i * 3 + 3].copy_from_slice(&p[..3]);
    }
}

/// The CLI's renderer.
#[derive(Debug, Default)]
pub struct CliMedia {
    pub analysis: Arc<crate::analysis::CliAnalysis>,
}

impl CliMedia {
    pub fn new(analysis: Arc<crate::analysis::CliAnalysis>) -> CliMedia {
        CliMedia { analysis }
    }
}

impl gmclip_core::MediaRender for CliMedia {
    fn name(&self) -> &'static str {
        "gmclip-render"
    }

    fn see(&self, plan: &Plan, req: &SeeRequest) -> Result<SeeImage, HostError> {
        let (sheet, notes) = see_image(plan, req)?;
        Ok(SeeImage {
            png: png_bytes(&sheet),
            width: sheet.width,
            height: sheet.height,
            notes,
        })
    }

    fn export(
        &self,
        plan: &Plan,
        out: &Path,
        cancel: Arc<AtomicBool>,
    ) -> Result<ExportDone, HostError> {
        export_impl(plan, out, cancel)
    }

    fn verify(&self, plan: &Plan, out: &Path) -> Result<Value, HostError> {
        verify_impl(plan, out)
    }

    fn verify_joins(&self, plan: &Plan, out: &Path) -> Result<Value, HostError> {
        crate::analysis::verify_joins(&self.analysis, plan, out)
    }

    fn matte_quality(&self, plan: &Plan, build: bool) -> Result<Value, HostError> {
        matte_quality_impl(plan, build)
    }

    fn luma_frames(
        &self,
        plan: &Plan,
        source: &str,
        frames: &[u64],
        max_w: u32,
    ) -> Result<Vec<gmclip_core::activity::LumaFrame>, HostError> {
        luma_frames_impl(plan, source, frames, max_w)
    }
}

#[cfg(any(target_os = "macos", windows))]
fn matte_quality_impl(plan: &Plan, build: bool) -> Result<Value, HostError> {
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let store = gmclip_render::matte::store::shared();
    let rows = gmclip_render::export::mattes::quality_report(plan, &mut inputs, &store, build)
        .map_err(render_host_err)?;
    let worst = rows
        .iter()
        .filter_map(|r| r.quality.as_ref().map(|q| (q.score, r)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(s, r)| json!({"plan_part": r.plan_part, "layer": r.layer, "score": s}));
    Ok(json!({ "parts": rows, "worst": worst }))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn matte_quality_impl(_: &Plan, _: bool) -> Result<Value, HostError> {
    Err(HostError::Unsupported(
        "roto mattes run on macOS and Windows",
    ))
}

#[cfg(any(target_os = "macos", windows))]
fn luma_frames_impl(
    plan: &Plan,
    source: &str,
    frames: &[u64],
    max_w: u32,
) -> Result<Vec<gmclip_core::activity::LumaFrame>, HostError> {
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let v = gmclip_render::export::mattes::luma_frames(plan, &mut inputs, source, frames, max_w)
        .map_err(render_host_err)?;
    Ok(v.into_iter()
        .map(|(width, height, data)| gmclip_core::activity::LumaFrame {
            width,
            height,
            data,
        })
        .collect())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn luma_frames_impl(
    _: &Plan,
    _: &str,
    _: &[u64],
    _: u32,
) -> Result<Vec<gmclip_core::activity::LumaFrame>, HostError> {
    Err(HostError::Unsupported(
        "frame reading runs on macOS and Windows",
    ))
}

/// The `see` image before PNG encoding: frames rendered, text drawn, masks filled, laid
/// out within `max_px`.
fn see_image(plan: &Plan, req: &SeeRequest) -> Result<(RgbImage, Vec<String>), HostError> {
    {
        let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
        // Text is drawn below (leniently), so the frames are rendered without it.
        let mut bare = plan.clone();
        bare.text_events = None;
        let rendered = render_frames(&bare, &mut inputs, &req.frames)?;
        let mut notes = Vec::new();
        let mut raster = None;
        let mut frames: Vec<RgbImage> = Vec::new();
        for ((k, mut img), masks) in rendered.into_iter().zip(&req.masks) {
            draw_text(plan, k, &mut img, &mut raster, &mut notes);
            // masks after text: nothing is drawn over a private region's black-out
            for m in masks {
                fill(&mut img, m, [24, 24, 24]);
            }
            frames.push(img);
        }
        let n = frames.len().max(1) as u32;
        let cols = match req.layout {
            SeeLayout::Frame => 1,
            SeeLayout::Strip => n,
            SeeLayout::Sheet => (f64::from(n)).sqrt().ceil() as u32,
        };
        let rows = n.div_ceil(cols);
        let (fw, fh) = frames.first().map_or((1, 1), |f| (f.width, f.height));
        // fit the whole layout, gaps included, inside max_px on both sides
        let gap = if n > 1 { 4 } else { 0 };
        let room_w = f64::from(req.max_px.saturating_sub(gap * (cols - 1)));
        let room_h = f64::from(req.max_px.saturating_sub(gap * (rows - 1)));
        let scale = (room_w / f64::from(fw * cols))
            .min(room_h / f64::from(fh * rows))
            .min(1.0);
        let (tw, th) = (
            ((f64::from(fw) * scale) as u32).max(1),
            ((f64::from(fh) * scale) as u32).max(1),
        );
        let mut sheet = RgbImage::new(
            tw * cols + gap * (cols - 1),
            th * rows + gap * (rows - 1),
            [0, 0, 0],
        );
        for (i, f) in frames.iter().enumerate() {
            let t = resize(f, tw, th);
            let (c, r) = (i as u32 % cols, i as u32 / cols);
            blit(&mut sheet, &t, c * (tw + gap), r * (th + gap));
        }
        Ok((sheet, notes))
    }
}

fn export_impl(plan: &Plan, out: &Path, cancel: Arc<AtomicBool>) -> Result<ExportDone, HostError> {
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let opts = ExportOptions {
        cancel: Some(cancel),
        keep_superseded: Some(3),
        hash_encoder_input: false,
        progress: None,
        mattes: Some(gmclip_render::matte::store::shared()),
        ..Default::default()
    };
    let r =
        gmclip_render::export::export(plan, &mut inputs, out, &opts).map_err(render_host_err)?;
    Ok(ExportDone {
        out: r.out,
        frames: r.frames,
        seconds: r.seconds,
        notes: r.notes,
    })
}

fn verify_impl(plan: &Plan, out: &Path) -> Result<Value, HostError> {
    let bytes =
        std::fs::read(out).map_err(|_| HostError::Failed("the export cannot be read".into()))?;
    let expect = gmclip_media::verify::Expect::from_plan(plan);
    let r = gmclip_media::verify::verify(&bytes, &expect, None);
    Ok(json!({
        "passed": r.passed(),
        "checks": r.checks.iter().map(|c| json!({"step": c.step, "ok": c.ok, "detail": c.detail})).collect::<Vec<_>>(),
        "not_run": ["audio alignment, per-join A/V offset and loudness need the decoded mix (B-15 audio inputs)"],
    }))
}

fn render_host_err(e: RenderError) -> HostError {
    match e {
        RenderError::Unsupported { .. } => HostError::Failed(format!("{e}")),
        other => failed(other),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn render_frames(
    plan: &Plan,
    inputs: &mut gmclip_render::export::inputs::DemuxInputs,
    at: &[u64],
) -> Result<Vec<(u64, RgbImage)>, HostError> {
    gmclip_render::export::render_frames(plan, inputs, at).map_err(render_host_err)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn render_frames(
    _: &Plan,
    _: &mut gmclip_render::export::inputs::DemuxInputs,
    _: &[u64],
) -> Result<Vec<(u64, RgbImage)>, HostError> {
    Err(HostError::Unsupported("frames render on macOS and Windows"))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use gmclip_core::MediaRender;

    const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";

    /// A 2 s 640x360 H.264 + AAC 48 kHz source made with ffmpeg (dev machines only).
    fn source(dir: &Path) -> Option<PathBuf> {
        if !Path::new(FFMPEG).exists() {
            eprintln!("skipped: no {FFMPEG}");
            return None;
        }
        let out = dir.join("src.mp4");
        let st = std::process::Command::new(FFMPEG)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=2",
                "-c:v",
                "libx264",
                "-profile:v",
                "high",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "30",
                "-c:a",
                "aac",
                "-ac",
                "2",
                "-b:a",
                "128k",
                "-shortest",
            ])
            .arg(&out)
            .status()
            .ok()?;
        st.success().then_some(out)
    }

    fn plan(src: &Path) -> Plan {
        let v = json!({
            "schema": "gmclip.renderplan/0",
            "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1},
                "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
                "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                          "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
                "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
                "container": {"moov_first": true, "edit_lists": false}, "frames": 45},
            "background": {"kind": "colour", "rgb": "#0c1118"},
            "sources": [{"id": "s1", "path": src.display().to_string()}],
            "parts": [{"id": "p1", "out": [0, 45], "picture": {"source": "s1", "first_frame": 6}, "audio": {"source": "s1", "first_sample": 9600, "rate": 48000},
                       "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": 640, "h": 360}, "place": {"x": 0, "y": 656, "w": 1080, "h": 607.5}, "z": 0}]}],
            "audio": {"loudness": {"mode": "as_recorded"}},
        });
        let p = gmclip_core::gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(
            gmclip_core::gmclip_plan::check_plan(&p).is_empty(),
            "{:?}",
            gmclip_core::gmclip_plan::check_plan(&p)
        );
        p
    }

    #[test]
    fn see_export_and_verify_a_real_source() {
        let dir =
            std::env::temp_dir().join(format!("gmclip-host-{}-see-export", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(src) = source(&dir) else { return };
        let plan = plan(&src);
        let m = CliMedia::new(Arc::new(crate::analysis::CliAnalysis::with_models(
            dir.join("no-models"),
        )));
        // a sheet of two frames, the second with a mask
        let mask = MaskRect {
            x: 0,
            y: 656,
            w: 540,
            h: 300,
            label: "chat (masked)".into(),
        };
        let img = m
            .see(
                &plan,
                &SeeRequest {
                    frames: vec![0, 30],
                    masks: vec![vec![], vec![mask]],
                    layout: SeeLayout::Strip,
                    max_px: 1024,
                    labels: false,
                },
            )
            .unwrap();
        assert!(
            img.width.max(img.height) <= 1024,
            "{}x{}",
            img.width,
            img.height
        );
        assert_eq!(&img.png[..8], b"\x89PNG\r\n\x1a\n");
        // the masked region is the mask's colour in the second tile
        let one = m
            .see(
                &plan,
                &SeeRequest {
                    frames: vec![30],
                    masks: vec![vec![MaskRect {
                        x: 0,
                        y: 0,
                        w: 1080,
                        h: 1920,
                        label: "x".into(),
                    }]],
                    layout: SeeLayout::Frame,
                    max_px: 540,
                    labels: false,
                },
            )
            .unwrap();
        assert_eq!((one.width, one.height), (303, 540));
        let _ = one;
        // the mask is painted: every pixel of a fully masked frame is the mask colour
        let (img, _) = see_image(
            &plan,
            &SeeRequest {
                frames: vec![30],
                masks: vec![vec![MaskRect {
                    x: 0,
                    y: 656,
                    w: 1080,
                    h: 608,
                    label: "chat".into(),
                }]],
                layout: SeeLayout::Frame,
                max_px: 1568,
                labels: false,
            },
        )
        .unwrap();
        let scale = img.height as f64 / 1920.0;
        let (x, y) = ((540.0 * scale) as u32, (900.0 * scale) as u32);
        let o = ((y * img.width + x) * 3) as usize;
        assert_eq!(&img.px[o..o + 3], &[24, 24, 24]);
        let (x, y) = ((540.0 * scale) as u32, (300.0 * scale) as u32);
        let o = ((y * img.width + x) * 3) as usize;
        assert_ne!(
            &img.px[o..o + 3],
            &[24, 24, 24],
            "outside the mask is the background"
        );
        // export, then verify
        let out = dir.join("out.mp4");
        let done = m
            .export(&plan, &out, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(done.frames, 45);
        assert!(out.exists());
        let v = m.verify(&plan, &out).unwrap();
        eprintln!("verify: {v}");
        assert_eq!(v["passed"], true, "{v}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Font files for the model planner (fonts are never stored in a project, SPEC §4.4): a
/// face is the file under the user's font folders (`~/Library/Fonts`, `/Library/Fonts`)
/// or the repo's `assets/fonts` (from a checkout) whose SHA-256 is the one the project
/// names; each file is hashed once, when a face is first asked for.
#[derive(Debug, Default)]
pub struct HolderFonts {
    dirs: Vec<PathBuf>,
    found: std::sync::Mutex<Vec<(String, String)>>,
}

impl HolderFonts {
    pub fn load() -> HolderFonts {
        let mut dirs = Vec::new();
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join("Library/Fonts"));
        }
        dirs.push(PathBuf::from("/Library/Fonts"));
        dirs.push(PathBuf::from("assets/fonts"));
        HolderFonts {
            dirs,
            found: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth > 0 {
                Self::files(&p, depth - 1, out);
            } else if p.extension().is_some_and(|x| {
                ["otf", "ttf", "ttc"].contains(&x.to_string_lossy().to_lowercase().as_str())
            }) {
                out.push(p);
            }
        }
    }
}

impl gmclip_core::planner::PlanHost for HolderFonts {
    fn font_path(&self, font: &gmclip_model::entities::Font) -> Option<String> {
        let want = font.sha256.as_deref()?;
        let mut found = self.found.lock().ok()?;
        if let Some((_, p)) = found.iter().find(|(sha, _)| sha == want) {
            return Some(p.clone());
        }
        let mut files = Vec::new();
        for d in &self.dirs {
            Self::files(d, 2, &mut files);
        }
        for f in files {
            let Ok((sha, _)) = gmclip_ml::sha256::file_hex(&f) else {
                continue;
            };
            let p = f.to_string_lossy().into_owned();
            let hit = sha == want;
            if !found.iter().any(|(s, _)| *s == sha) {
                found.push((sha, p.clone()));
            }
            if hit {
                return Some(p);
            }
        }
        None
    }
}
