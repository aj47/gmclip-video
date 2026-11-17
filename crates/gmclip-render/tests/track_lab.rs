//! Motion tracking (decision 0031, lab: Apple Vision): a 40 px white square crossing a
//! 640×360 picture of colour bars at 130 px/s (ffmpeg-made, a dev tool; the square carries
//! a grid so it has texture) is followed from its centre on the first frame; the point
//! stays within 9 px of the square's true centre (mean under 4.5 px) over the whole span.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;

use gmclip_plan::Plan;
use gmclip_render::export::DemuxInputs;
use gmclip_render::export::track::track_point;
use serde_json::json;

const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Apple Vision and ffmpeg (run with --features lab)"
)]
fn a_moving_square_is_followed() {
    if !Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gmclip-track-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("square.mp4");
    let out = Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("smptehdbars=s=640x360:r=30:d=3")
        .args(["-f", "lavfi", "-i", "color=c=white:s=40x40:r=30:d=3"])
        .args([
            "-filter_complex",
            "[1]drawgrid=w=10:h=10:t=3:c=black[sq];[0][sq]overlay=x='100+t*130':y=160,format=yuv420p",
        ])
        .args(["-c:v", "libx264", "-crf", "12", "-bf", "0"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json!({
        "schema": "gmclip.renderplan/0",
        "output": {"canvas": {"w": 640, "h": 360}, "fps": {"num": 30, "den": 1}, "frames": 90,
            "video": {"codec": "h264"}, "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "s1", "path": src.display().to_string()}],
        "parts": [{"id": "p1", "out": [0, 90], "picture": {"source": "s1", "first_frame": 0},
            "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
            "layers": [{"id": "l", "role": "screen", "crop": {"x": 0, "y": 0, "w": 640, "h": 360},
                        "place": {"x": 0, "y": 0, "w": 640, "h": 360}, "z": 0}]}],
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    let plan: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    // the square's centre on frame f: (120 + 130·f/30, 180) px
    let truth = |f: u64| (120.0 + 130.0 * f as f64 / 30.0, 180.0);
    let (x0, y0) = truth(0);
    let t0 = std::time::Instant::now();
    let run = track_point(
        &plan,
        &mut DemuxInputs::new(None),
        "s1",
        0,
        80,
        [x0 / 640.0, y0 / 360.0],
        0.2,
        None,
    )
    .unwrap();
    eprintln!(
        "tracked {} frames in {:.2} s, lost at {:?}",
        run.samples.len(),
        t0.elapsed().as_secs_f64(),
        run.lost_at
    );
    assert_eq!(run.size, (640, 360));
    assert_eq!(run.lost_at, None);
    assert_eq!(run.samples.len(), 81);
    let errs: Vec<f64> = run
        .samples
        .iter()
        .map(|s| {
            let (tx, ty) = truth(s.frame);
            ((s.x * 640.0 - tx).powi(2) + (s.y * 360.0 - ty).powi(2)).sqrt()
        })
        .collect();
    let worst = errs.iter().copied().fold(0.0f64, f64::max);
    let mean = errs.iter().sum::<f64>() / errs.len() as f64;
    eprintln!("error: mean {mean:.1} px, worst {worst:.1} px (on 640×360)");
    // Vision's box jitters a few pixels frame to frame; it never drifts off
    assert!(
        worst < 9.0 && mean < 4.5,
        "mean {mean:.1}, worst {worst:.1} px"
    );
    assert!(run.samples.iter().all(|s| s.confidence >= 0.5));
    let _ = std::fs::remove_dir_all(&dir);
}
