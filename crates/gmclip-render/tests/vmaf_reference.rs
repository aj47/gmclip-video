//! H.07 lab helper (R-EXP-3): the reference for the VMAF table. Renders every output frame
//! of a plan through the compositor (no encoder in the path) and pipes them to ffmpeg's
//! x264 `-preset slow -crf 14` (ffmpeg is a dev tool here, never shipped). The export under
//! test is then scored against this file by `tools/h07_vmaf.sh`.
//!
//!   GMCLIP_VMAF_PLAN=<plan.json> GMCLIP_VMAF_REF=<out.mp4> \
//!     cargo test --release -p gmclip-render --test vmaf_reference -- --ignored

#![cfg(target_os = "macos")]

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "lab: H.07 reference encode (ffmpeg, the plan's media)"]
fn x264_crf14_reference() {
    let (Ok(plan), Ok(out)) = (
        std::env::var("GMCLIP_VMAF_PLAN"),
        std::env::var("GMCLIP_VMAF_REF"),
    ) else {
        eprintln!("skipped: set GMCLIP_VMAF_PLAN and GMCLIP_VMAF_REF");
        return;
    };
    let report = gmclip_plan::check(&std::fs::read(&plan).unwrap());
    assert!(report.errors.is_empty(), "{:?}", report.errors.first());
    let plan = report.plan.unwrap();
    let (w, h) = (plan.output.canvas.w.get(), plan.output.canvas.h.get());
    let fps = plan.output.fps.num.get().to_string(); // den is always 1
    let mut ff = Command::new("/opt/homebrew/bin/ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24"])
        .args(["-s", &format!("{w}x{h}"), "-r", &fps, "-i", "-"])
        .args([
            "-vf",
            "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p",
        ])
        .args(["-c:v", "libx264", "-preset", "slow", "-crf", "14"])
        .args([
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            &out,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .expect("ffmpeg");
    let mut pipe = ff.stdin.take().unwrap();
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let n = plan.output.frames;
    let mut at = 0;
    while at < n {
        let batch: Vec<u64> = (at..(at + 30).min(n)).collect();
        for (_, img) in gmclip_render::export::render_frames(&plan, &mut inputs, &batch).unwrap() {
            pipe.write_all(&img.px).unwrap();
        }
        at += 30;
    }
    drop(pipe);
    assert!(ff.wait().unwrap().success());
    eprintln!("{n} frames → {out}");
}
