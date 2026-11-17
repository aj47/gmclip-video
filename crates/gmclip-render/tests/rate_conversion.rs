//! Picture rate conversion (decision 0017): a 60 fps, a 23.976 fps and a VFR source cut
//! into one 30 fps output. Every source frame is a different flat grey, so the frame an
//! output frame shows is read back from its grey. The expected pick is computed from the
//! source as ffmpeg decodes it (its own PTS and greys, an independent decoder): the last
//! frame presented at or before the output frame's source time. Checked on the compose
//! (`render_frames`, the preview's body) and on the exported MP4.
//!
//! Sources are made with ffmpeg (a dev tool, never shipped); skipped without it.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

use gmclip_plan::Plan;
use gmclip_render::export::{DemuxInputs, ExportOptions, export, render_frames};
use serde_json::json;

const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
const FFPROBE: &str = "/opt/homebrew/bin/ffprobe";
const W: usize = 320;
const H: usize = 180;

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A 3 s source whose frame `n` is grey `16 + 7n mod 220`, with a 48 kHz tone. `pts`:
/// an ffmpeg `setpts` expression (VFR), else CFR at `rate`.
fn source(dir: &Path, name: &str, rate: &str, pts: Option<&str>) -> PathBuf {
    let out = dir.join(name);
    let mut vf = String::from("geq=lum='16+mod(N*7\\,220)':cb=128:cr=128");
    if let Some(p) = pts {
        vf.push_str(&format!(",setpts='{p}'"));
    }
    run(Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "color=c=black:s={W}x{H}:r={rate}:d=3,format=yuv420p"
        ))
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
        ])
        .args(["-vf", &vf, "-fps_mode", "passthrough"])
        .args([
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-crf",
            "6",
            "-bf",
            "0",
        ])
        .args([
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", "-b:a", "128k",
        ])
        .args(["-video_track_timescale", "90000", "-shortest"])
        .arg(&out));
    out
}

/// `(pts in time-base ticks, timebase den)` of every frame in presentation order, and
/// each frame's mean grey, as ffmpeg decodes the file.
fn decoded(path: &Path) -> (Vec<i64>, u64, Vec<f64>) {
    let tb = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=time_base",
            "-of",
            "csv=p=0",
        ])
        .arg(path)))
    .unwrap();
    let den: u64 = tb.trim().split('/').nth(1).unwrap().parse().unwrap();
    let csv = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pts",
            "-of",
            "csv=p=0",
        ])
        .arg(path)))
    .unwrap();
    let mut pts: Vec<i64> = csv
        .lines()
        .filter_map(|l| l.split(',').next().and_then(|v| v.trim().parse().ok()))
        .collect();
    pts.sort_unstable();
    let greys = greys(path, W, H);
    assert_eq!(greys.len(), pts.len());
    (pts, den, greys)
}

/// The mean grey (Y) of each decoded frame of `path`'s video, in presentation order.
fn greys(path: &Path, w: usize, h: usize) -> Vec<f64> {
    let raw = run(Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
        ])
        .args(["-pix_fmt", "gray", "-"]));
    raw.chunks_exact(w * h).map(mean_centre(w, h)).collect()
}

/// Mean of the centre half of a `w`×`h` single-channel picture.
fn mean_centre(w: usize, h: usize) -> impl Fn(&[u8]) -> f64 {
    move |px: &[u8]| {
        let (mut s, mut n) = (0u64, 0u64);
        for y in h / 4..3 * h / 4 {
            for x in w / 4..3 * w / 4 {
                s += u64::from(px[y * w + x]);
                n += 1;
            }
        }
        s as f64 / n as f64
    }
}

/// The source frame whose grey is nearest `g`.
fn which(greys: &[f64], g: f64) -> usize {
    (0..greys.len())
        .min_by(|&a, &b| (greys[a] - g).abs().total_cmp(&(greys[b] - g).abs()))
        .unwrap()
}

/// The last frame presented at or before `t = num / den` s (PTS rebased on the first).
fn at_or_before(pts: &[i64], tb: u64, num: u128, den: u128) -> usize {
    let p0 = pts[0];
    // t in ticks, nearest (decision 0017's rounding)
    let ticks = ((2 * num * u128::from(tb) + den) / (2 * den)) as i64;
    pts.iter().rposition(|&p| p - p0 <= ticks).unwrap()
}

struct Src {
    path: PathBuf,
    rate: (u64, u64),
    pts: Vec<i64>,
    tb: u64,
    greys: Vec<f64>,
}

#[test]
fn sixty_film_and_vfr_into_thirty_pick_by_source_time() {
    if !Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gmclip-rate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mk = |name: &str, rate: &str, pts: Option<&str>, nominal: (u64, u64)| {
        let path = source(&dir, name, rate, pts);
        let (pts, tb, greys) = decoded(&path);
        Src {
            path,
            rate: nominal,
            pts,
            tb,
            greys,
        }
    };
    let srcs = [
        mk("s60.mp4", "60", None, (60, 1)),
        mk("s23976.mp4", "24000/1001", None, (24000, 1001)),
        // VFR, nominally 60: every fifth frame held three times as long
        mk("vfr.mp4", "60", Some("(N+2*floor(N/5))/60/TB"), (60, 1)),
    ];
    // parts: (source, first_frame on the 30 fps source clock, output frames)
    let cuts = [(0usize, 10u64, 20u64), (1, 7, 20), (2, 3, 25)];
    let mut parts = Vec::new();
    let mut at = 0u64;
    for (i, &(s, f0, n)) in cuts.iter().enumerate() {
        parts.push(json!({
            "id": format!("p{}", i + 1), "out": [at, at + n],
            "picture": {"source": format!("s{}", s + 1), "first_frame": f0,
                        "fps": {"num": srcs[s].rate.0, "den": srcs[s].rate.1}},
            "audio": {"source": format!("s{}", s + 1), "first_sample": f0 * 1600, "rate": 48000},
            "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": W, "h": H},
                        "place": {"x": 0, "y": 0, "w": 640, "h": 360}, "z": 0}]
        }));
        at += n;
    }
    let frames = at;
    let sources: Vec<_> = srcs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({"id": format!("s{}", i + 1), "path": s.path.display().to_string(),
                   "expect": {"video": {"fps": {"num": s.rate.0, "den": s.rate.1}}}})
        })
        .collect();
    let v = json!({
        "schema": "gmclip.renderplan/1",
        "output": {"canvas": {"w": 640, "h": 360}, "fps": {"num": 30, "den": 1},
            "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
            "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                      "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
            "container": {"moov_first": true, "edit_lists": false}, "frames": frames},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": sources,
        "parts": parts,
        "audio": {"loudness": {"mode": "as_recorded"}},
    });
    let plan: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );

    // what each output frame must show, from the independently decoded sources
    let mut want = Vec::new();
    for &(s, f0, n) in &cuts {
        let src = &srcs[s];
        for k in 0..n {
            let i = at_or_before(&src.pts, src.tb, u128::from(f0 + k), 30);
            want.push((s, i));
        }
    }
    // the VFR part really is not evenly spaced: the nominal pick differs somewhere
    let nominal_differs = (0..cuts[2].2).any(|k| {
        let i = want[(cuts[0].2 + cuts[1].2 + k) as usize].1 as u64;
        i != (cuts[2].1 + k) * 2
    });
    assert!(nominal_differs, "the VFR fixture is not VFR");
    // the 60 → 30 part drops every other frame; the 23.976 → 30 part repeats some
    assert_eq!(want[1].1, want[0].1 + 2);
    let film = &want[cuts[0].2 as usize..(cuts[0].2 + cuts[1].2) as usize];
    assert!(film.windows(2).any(|w| w[0] == w[1]));

    // the compose (preview and export share it)
    let all: Vec<u64> = (0..frames).collect();
    let mut inputs = DemuxInputs::new(None);
    let shots = render_frames(&plan, &mut inputs, &all).unwrap();
    for (f, img) in &shots {
        let (s, i) = want[*f as usize];
        // RGB grey, full range like ffmpeg's `gray` of the limited-range source
        let g: Vec<u8> = img.px.chunks_exact(3).map(|p| p[1]).collect();
        let y = mean_centre(img.width as usize, img.height as usize)(&g);
        assert_eq!(
            which(&srcs[s].greys, y),
            i,
            "compose: output frame {f} (grey {y:.1})"
        );
    }

    // the export, decoded by ffmpeg
    let out = dir.join("out.mp4");
    let mut inputs = DemuxInputs::new(None);
    let r = export(&plan, &mut inputs, &out, &ExportOptions::default()).unwrap();
    assert_eq!(r.frames, frames);
    let got = greys(&out, 640, 360);
    assert_eq!(got.len() as u64, frames);
    for (f, &y) in got.iter().enumerate() {
        let (s, i) = want[f];
        assert_eq!(
            which(&srcs[s].greys, y),
            i,
            "export: output frame {f} (grey {y:.1})"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `(source, first_frame, frames, picture.fps)`.
type NtscCut = (usize, u64, u64, Option<(u64, u64)>);

/// Decision 0020: a 29.97 fps output. A 29.97 source maps frame for frame; a 30 fps one
/// converts into it. The file is 30000/1001 with exactly S(frames) samples (export's own
/// verify), and every frame shows the expected source frame.
#[test]
fn a_29_97_output_maps_ntsc_frame_for_frame_and_converts_30() {
    if !Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gmclip-ntsc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ntsc = source(&dir, "s2997.mp4", "30000/1001", None);
    let thirty = source(&dir, "s30.mp4", "30", None);
    let (np, ntb, ng) = decoded(&ntsc);
    let (tp, ttb, tg) = decoded(&thirty);
    // (source, first_frame, frames, picture.fps)
    let cuts: [NtscCut; 2] = [(0, 11, 31, None), (1, 5, 30, Some((30, 1)))];
    let srcs = [(&ntsc, (30000u64, 1001u64)), (&thirty, (30, 1))];
    let mut parts = Vec::new();
    let mut at = 0u64;
    for (i, &(s, f0, n, fps)) in cuts.iter().enumerate() {
        // the part's time on the 29.97 clock, in samples (rounded down)
        let first_sample = f0 * 48_000 * 1001 / 30_000;
        let mut picture = json!({"source": format!("s{}", s + 1), "first_frame": f0});
        if let Some((a, b)) = fps {
            picture["fps"] = json!({"num": a, "den": b});
        }
        parts.push(json!({
            "id": format!("p{}", i + 1), "out": [at, at + n], "picture": picture,
            "audio": {"source": format!("s{}", s + 1), "first_sample": first_sample, "rate": 48000,
                      "fade_in": {"samples": 144, "shape": "linear"}, "fade_out": {"samples": 144, "shape": "linear"}},
            "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": W, "h": H},
                        "place": {"x": 0, "y": 0, "w": 640, "h": 360}, "z": 0}]
        }));
        at += n;
    }
    let frames = at;
    let sources: Vec<_> = srcs
        .iter()
        .enumerate()
        .map(|(i, (p, r))| {
            json!({"id": format!("s{}", i + 1), "path": p.display().to_string(),
                   "expect": {"video": {"fps": {"num": r.0, "den": r.1}}}})
        })
        .collect();
    let v = json!({
        "schema": "gmclip.renderplan/1",
        "output": {"canvas": {"w": 640, "h": 360}, "fps": {"num": 30000, "den": 1001},
            "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
            "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                      "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
            "container": {"moov_first": true, "edit_lists": false}, "frames": frames},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": sources,
        "parts": parts,
        "audio": {"loudness": {"mode": "as_recorded"}},
    });
    let plan: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );
    let mut want = Vec::new();
    for &(s, f0, n, fps) in &cuts {
        for k in 0..n {
            let i = match fps {
                // same rate: frame for frame
                None => (f0 + k) as usize,
                // 30 into 29.97: at or before (f0 + k)·1001/30000 s
                Some(_) => at_or_before(&tp, ttb, u128::from(f0 + k) * 1001, 30_000),
            };
            want.push((s, i));
        }
    }
    let out = dir.join("out.mp4");
    let mut inputs = DemuxInputs::new(None);
    let r = export(&plan, &mut inputs, &out, &ExportOptions::default()).unwrap();
    assert_eq!(r.frames, frames);
    let rate = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate,nb_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)))
    .unwrap();
    assert_eq!(rate.trim(), format!("30000/1001,{frames}"));
    let got = greys(&out, 640, 360);
    assert_eq!(got.len() as u64, frames);
    let g = [&ng, &tg];
    for (f, &y) in got.iter().enumerate() {
        let (s, i) = want[f];
        assert_eq!(which(g[s], y), i, "export: output frame {f} (grey {y:.1})");
    }
    let _ = (np, ntb);
    let _ = std::fs::remove_dir_all(&dir);
}
