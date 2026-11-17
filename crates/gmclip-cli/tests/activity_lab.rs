//! R.03 on real footage (lab: VideoToolbox, AJ's media): the activity crop of h79's
//! part 3 ("terrible prompt": AJ types a prompt into ComfyUI on the 1440p screen),
//! frames read at 2 fps with the renderer's `luma_frames` (what the CLI host's
//! `MediaRender::luma_frames` calls), analysed by core.

#![cfg(target_os = "macos")]

use gmclip_core::activity::{CropRules, NRect, analyse};

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + AJ's media (run with --features lab)"
)]
fn activity_crop_on_h79_typing() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Ok(bytes) = std::fs::read(repo.join("schemas/examples/h79-parts-1-3.renderplan.json"))
    else {
        return;
    };
    let mut plan = gmclip_core::gmclip_plan::parse(&bytes).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    if !std::path::Path::new(&plan.sources[0].path).is_file() {
        eprintln!("skipped: h79's source is not on this machine");
        return;
    }
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    for p in &plan.parts {
        let screen = p
            .layers
            .iter()
            .find(|l| l.role.as_str() == "screen")
            .unwrap();
        let n = p.out.end() - p.out.start();
        let frames: Vec<u64> = (0..n)
            .step_by(15)
            .map(|i| p.picture.first_frame + i)
            .collect();
        let t = std::time::Instant::now();
        let luma: Vec<gmclip_core::activity::LumaFrame> =
            gmclip_render::export::mattes::luma_frames(
                &plan,
                &mut inputs,
                p.picture.source.as_str(),
                &frames,
                640,
            )
            .unwrap()
            .into_iter()
            .map(|(width, height, data)| gmclip_core::activity::LumaFrame {
                width,
                height,
                data,
            })
            .collect();
        let read_ms = t.elapsed().as_secs_f64() * 1000.0;
        let rules = CropRules {
            region: NRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            // the camera inset is not screen (as crop_follow_action excludes other layers)
            exclude: p
                .layers
                .iter()
                .filter(|l| l.role.as_str() == "cam")
                .map(|l| NRect {
                    x: l.crop.x / 2560.0,
                    y: l.crop.y / 1440.0,
                    w: l.crop.w / 2560.0,
                    h: l.crop.h / 1440.0,
                })
                .collect(),
            aspect: screen.crop.w / screen.crop.h,
            source: (2560, 1440),
            panel_w: screen.place.w,
        };
        let t = std::time::Instant::now();
        let a = analyse(&luma, &rules, &[]).unwrap();
        let an_ms = t.elapsed().as_secs_f64() * 1000.0;
        let Some(c) = a.crops.first() else {
            eprintln!(
                "{}: still screen (change {}), crop kept",
                p.id.as_str(),
                a.change
            );
            continue;
        };
        eprintln!(
            "{}: {} samples ({}x{}) read in {read_ms:.0} ms, analysed in {an_ms:.1} ms; AJ's crop px {:?}; activity crop px x {:.0} y {:.0} w {:.0} h {:.0}, holds {:.2}, scale {:.2}, moved {:.2}, {} crop(s), lint {}",
            p.id.as_str(),
            luma.len(),
            luma[0].width,
            luma[0].height,
            (screen.crop.x, screen.crop.y, screen.crop.w, screen.crop.h),
            c.crop.x * 2560.0,
            c.crop.y * 1440.0,
            c.crop.w * 2560.0,
            c.crop.h * 1440.0,
            c.held,
            c.scale,
            a.moved,
            a.crops.len(),
            a.text_too_small
        );
        assert!(c.scale >= 0.6 - 1e-9 || a.text_too_small);
        assert!(c.crop.w * 2560.0 <= 2560.0 / 1.6 + 1.0);
    }
}
