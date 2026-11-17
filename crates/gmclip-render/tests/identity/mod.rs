//! The fixture cut of the preview = export guard (decision 0053), shared by
//! `gmclip-render`'s `preview_export_identity` (frames) and the app's
//! `playback_export_identity` (playback's mix): two small ffmpeg-made sources (a dev tool,
//! never shipped) and a six-part cut over them with every kind of join the viewer plays
//! through. A feature that changes what preview or playback draws or mixes belongs in
//! this plan, so the guard covers it.
//!
//! - **(a):** 320 × 180 `testsrc2` at 30 fps, H.264 with B-frames (GOP 30), stereo AAC
//!   (440 Hz left, 660 Hz right: a channel swap shows).
//! - **(b):** 320 × 180 `testsrc` at 30 fps, H.264 without B-frames (GOP 15), mono AAC
//!   (880 Hz, played on both sides).
//! - **Joins:** across sources, a jump forward past a keyframe, a jump back, a continuous
//!   join (no de-click), and a part whose picture and sound come from different sources.
//!   A gain envelope and a caption each cross a join; one part has its own gain.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use gmclip_plan::Plan;
use serde_json::json;

pub const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
const W: u32 = 320;
const H: u32 = 180;
/// The bundled OFL face and its hash (as `overlay_goldens`).
const FIGTREE: &str = "assets/fonts/figtree/Figtree-Black.ttf";
const FIGTREE_SHA: &str = "d32cf04815985f3cb0fa811236a1e130db44885ea981927a3237427320e851ef";

/// `(picture source, first picture frame, audio source, first audio frame, frames, gain dB)`.
type Cut = (&'static str, u64, &'static str, u64, u64, f64);

const CUTS: [Cut; 6] = [
    ("a", 0, "a", 0, 24, 0.0),
    // across sources
    ("b", 30, "b", 30, 24, 0.0),
    // a jump forward past (a)'s keyframe at 60, with its own gain
    ("a", 70, "a", 70, 24, -6.0),
    // a jump back
    ("a", 10, "a", 10, 20, 0.0),
    // continuous with the part before: no de-click
    ("a", 30, "a", 30, 24, 0.0),
    // (b)'s picture over (a)'s sound
    ("b", 60, "a", 90, 24, 0.0),
];

/// Writes the two sources into `dir`; `None` (and a skip line) without ffmpeg.
pub fn sources(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    if !Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return None;
    }
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join("a.mp4");
    let b = dir.join("b.mp4");
    make(
        &a,
        &format!("testsrc2=s={W}x{H}:r=30:d=4"),
        "aevalsrc=0.3*sin(2*PI*440*t)|0.3*sin(2*PI*660*t):s=48000:d=4",
        &["-bf", "2", "-g", "30", "-ac", "2"],
    );
    make(
        &b,
        &format!("testsrc=s={W}x{H}:r=30:d=4"),
        "sine=frequency=880:sample_rate=48000:duration=4",
        &["-bf", "0", "-g", "15", "-ac", "1"],
    );
    Some((a, b))
}

fn make(out: &Path, video: &str, audio: &str, extra: &[&str]) {
    let o = Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", video])
        .args(["-f", "lavfi", "-i", audio])
        .args(["-c:v", "libx264", "-profile:v", "high", "-crf", "18"])
        .args(extra)
        .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k"])
        .args(["-video_track_timescale", "90000", "-shortest"])
        .arg(out)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The fixture cut over sources `a` and `b`, at 360 × 640; `repo` is the repository root
/// (the bundled font). `trim`: frames taken off the third part's head (the cut before a
/// trim, for a session that is edited into the fixture).
pub fn plan(repo: &Path, a: &Path, b: &Path, trim: u64) -> Plan {
    let mut parts = Vec::new();
    let mut at = 0u64;
    for (i, &(pic, f0, snd, a0, n, gain)) in CUTS.iter().enumerate() {
        let (f0, a0, n) = if i == 2 {
            (f0 + trim, a0 + trim, n - trim)
        } else {
            (f0, a0, n)
        };
        let mut layers = vec![json!({"id": "screen", "role": "screen",
            "crop": {"x": 0, "y": 0, "w": W, "h": H},
            "place": {"x": 0, "y": 180, "w": 360, "h": 202.5}, "z": 0})];
        match i {
            // a 9:16 strip filling the canvas (fractional crop)
            1 | 5 => {
                layers[0]["crop"] = json!({"x": 109.375, "y": 0, "w": 101.25, "h": 180});
                layers[0]["place"] = json!({"x": 0, "y": 0, "w": 360, "h": 640});
            }
            // an inset of the same picture over it, half transparent
            2 | 3 => layers.push(json!({"id": "cam", "role": "cam",
                "crop": {"x": 200, "y": 100, "w": 80, "h": 60},
                "place": {"x": 232.5, "y": 440.25, "w": 112, "h": 84}, "z": 1, "opacity": 0.6})),
            _ => {}
        }
        parts.push(json!({
            "id": format!("p{}", i + 1), "out": [at, at + n],
            "picture": {"source": pic, "first_frame": f0},
            "audio": {"source": snd, "first_sample": a0 * 1600, "rate": 48000, "gain_db": gain},
            "layers": layers,
        }));
        at += n;
    }
    let frames = at;
    let j1 = CUTS[0].4;
    let j2 = j1 + CUTS[1].4;
    let font = repo.join(FIGTREE);
    let v = json!({
        "schema": "gmclip.renderplan/1",
        "output": {"canvas": {"w": 360, "h": 640}, "fps": {"num": 30, "den": 1},
            "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
            "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                      "rate": {"mode": "vbr", "target_bps": 4000000, "max_bps": 6000000}, "timescale": 15360},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
            "container": {"moov_first": true, "edit_lists": false}, "frames": frames},
        "background": {"kind": "colour", "rgb": "#0c1118"},
        "sources": [{"id": "a", "path": a.display().to_string()}, {"id": "b", "path": b.display().to_string()}],
        "fonts": [{"id": "figtree", "path": font.to_string_lossy(), "sha256": FIGTREE_SHA, "em_per_size": 0.8}],
        "text_styles": {"Speech": {"font": "figtree", "size": 40, "fill": "#FFFFFFFF",
            "outline": {"width": 5, "colour": "#000000FF"},
            "shadow": {"dx": 3, "dy": 3, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline"}}},
        "parts": parts,
        "text_events": [
            {"id": "c1", "out": [j2 - 10, j2 + 10], "layer": 1, "role": "caption", "style": "Speech",
             "pos": {"x": 180, "y": 520}, "align": 2, "size": 40,
             "runs": [{"text": "every join "}, {"text": "matches", "fill": "#FFD400FF"}],
             "anim": {"kind": "pop"}}
        ],
        "audio": {
            "loudness": {"mode": "as_recorded"},
            "envelopes": [{"id": "e1", "out_samples": [(j1 - 8) * 1600, (j1 + 8) * 1600], "gain_db": 6.0,
                           "ramp_in_samples": 1600, "ramp_out_samples": 2400, "curve": "linear_db"}]
        },
    });
    let mut plan: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    // de-click every side of a join that is not a continuation, and the cut's ends
    let n = plan.parts.len();
    for i in 0..n {
        let fade = gmclip_plan::types::Fade {
            samples: 144,
            shape: gmclip_plan::types::FadeShape::Linear,
        };
        let cont_in = i > 0 && gmclip_plan::validate::continuous_join(&plan, i - 1);
        let cont_out = i + 1 < n && gmclip_plan::validate::continuous_join(&plan, i);
        plan.parts[i].audio.fade_in = Some((!cont_in).then_some(fade));
        plan.parts[i].audio.fade_out = Some((!cont_out).then_some(fade));
    }
    let errs = gmclip_plan::check_plan(&plan);
    assert!(errs.is_empty(), "the fixture plan: {errs:?}");
    assert_eq!(plan.output.frames, frames);
    plan
}

/// The first output frame of every part after the first.
pub fn joins(plan: &Plan) -> Vec<u64> {
    plan.parts.iter().skip(1).map(|p| p.out.start()).collect()
}

/// Output frames the guard looks at as paused seeks: the cut's ends and two frames on
/// each side of every join, joins last-first (each a new decode chain).
pub fn seeks(plan: &Plan) -> Vec<u64> {
    let last = plan.output.frames - 1;
    let mut v = vec![0, last];
    for j in joins(plan).into_iter().rev() {
        v.extend([j - 2, j - 1, j, j + 1]);
    }
    v
}
