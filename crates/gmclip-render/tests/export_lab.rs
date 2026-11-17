//! H-10 (lab): `export` renders a RenderPlan to an MP4 on VideoToolbox + Metal.
//! Sources are opened through an ffprobe-driven shim until B-01/B-02 land.
#![cfg(target_os = "macos")]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Command;

use gmclip_media::backend::SampleSource;
use gmclip_media::index::SampleIdx;
use gmclip_media::{MediaError, SampleIndex};
use gmclip_plan::Plan;
use gmclip_render::compositor::ChromaSiting;
use gmclip_render::export::{AudioInput, ExportOptions, Inputs, VideoCodec, VideoInput, export};
use gmclip_render::ops::RenderError;

mod common;

const FFPROBE: &str = "/opt/homebrew/bin/ffprobe";
const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn lab_dir() -> PathBuf {
    let d = std::env::var_os("GMCLIP_LAB_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lab-media")
        });
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Probed {
    codec: String,
    width: u32,
    height: u32,
    extradata: Vec<u8>,
    index: SampleIndex,
    chroma: String,
}

fn probe(path: &str, stream: &str) -> Probed {
    let s = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_streams",
            "-show_data",
        ])
        .arg(path)))
    .unwrap();
    let field = |k: &str| {
        s.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or("")
            .to_owned()
    };
    let mut extradata = Vec::new();
    if let Some(i) = s.find("extradata=") {
        let t = &s[i + 10..];
        let end = t.find("\nextradata_size=").unwrap_or(t.len());
        for line in t[..end].lines() {
            let Some((_, rest)) = line.split_once(": ") else {
                continue;
            };
            let hex: String = rest
                .chars()
                .take(40)
                .filter(|c| c.is_ascii_hexdigit())
                .collect();
            for k in (0..hex.len() / 2 * 2).step_by(2) {
                extradata.push(u8::from_str_radix(&hex[k..k + 2], 16).unwrap());
            }
        }
    }
    let tb = field("time_base");
    let ts: u32 = tb.split_once('/').unwrap().1.parse().unwrap();
    let pk = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "packet=pts,dts,size,pos,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)))
    .unwrap();
    let mut idx = SampleIndex {
        timescale: ts,
        complete: true,
        ..Default::default()
    };
    for line in pk.lines() {
        let f: Vec<&str> = line.split(',').collect();
        let pts: i64 = f[0].parse().unwrap();
        let dts: i64 = f[1].parse().unwrap_or(pts);
        idx.dts.push(dts);
        idx.cts_off.push((pts - dts) as i32);
        idx.size.push(f[2].parse().unwrap());
        idx.offset.push(f[3].parse().unwrap());
        idx.sync.push(f[4].starts_with('K'));
    }
    if path.ends_with(".mkv") {
        // (Simple)Block header: 1-byte track vint, 16-bit timecode, flags.
        for o in idx.offset.iter_mut() {
            *o += 4;
        }
    }
    let mut order: Vec<u32> = (0..idx.dts.len() as u32).collect();
    order.sort_by_key(|&i| idx.dts[i as usize] + i64::from(idx.cts_off[i as usize]));
    idx.pts_order = order;
    Probed {
        codec: field("codec_name"),
        width: field("width").parse().unwrap_or(0),
        height: field("height").parse().unwrap_or(0),
        extradata,
        index: idx,
        chroma: field("chroma_location"),
    }
}

struct FileSource {
    file: File,
    index: SampleIndex,
}

impl SampleSource for FileSource {
    fn index(&self) -> &SampleIndex {
        &self.index
    }
    fn read(&mut self, s: SampleIdx, buf: &mut Vec<u8>) -> Result<(), MediaError> {
        let i = s.0 as usize;
        buf.resize(self.index.size[i] as usize, 0);
        self.file
            .seek(SeekFrom::Start(self.index.offset[i]))
            .unwrap();
        self.file.read_exact(buf).unwrap();
        Ok(())
    }
}

/// Probes each source once and hands out fresh file handles.
#[derive(Default)]
struct ShimInputs {
    video: std::collections::HashMap<String, std::rc::Rc<Probed>>,
    audio: std::collections::HashMap<String, std::rc::Rc<Probed>>,
}

fn path_of<'a>(plan: &'a Plan, id: &str) -> &'a str {
    &plan
        .sources
        .iter()
        .find(|s| s.id.as_str() == id)
        .unwrap()
        .path
}

impl Inputs for ShimInputs {
    fn video(&mut self, plan: &Plan, source: &str) -> Result<VideoInput, RenderError> {
        let path = path_of(plan, source).to_owned();
        let p = self
            .video
            .entry(source.to_owned())
            .or_insert_with(|| std::rc::Rc::new(probe(&path, "v:0")))
            .clone();
        let codec = match p.codec.as_str() {
            "h264" => VideoCodec::H264 {
                avcc: p.extradata.clone(),
            },
            "vp9" => VideoCodec::Vp9,
            c => panic!("codec {c}"),
        };
        Ok(VideoInput {
            source: Box::new(FileSource {
                file: File::open(&path).unwrap(),
                index: p.index.clone(),
            }),
            codec,
            width: p.width,
            height: p.height,
            // Tagged left, or untagged H.264 (chroma_loc_type 0): left; untagged VP9: centre.
            siting: if p.chroma == "left" || p.codec == "h264" {
                ChromaSiting::Left
            } else {
                ChromaSiting::Center
            },
        })
    }

    fn audio(&mut self, plan: &Plan, source: &str) -> Result<AudioInput, RenderError> {
        let path = path_of(plan, source).to_owned();
        let p = self
            .audio
            .entry(source.to_owned())
            .or_insert_with(|| std::rc::Rc::new(probe(&path, "a:0")))
            .clone();
        let first = p.index.dts[0] + i64::from(p.index.cts_off[0]);
        Ok(AudioInput {
            source: Box::new(FileSource {
                file: File::open(&path).unwrap(),
                index: p.index.clone(),
            }),
            asc: p.extradata.clone(),
            pcm: None,
            sample_rate: 48_000,
            channels: 2,
            skip: (-first).max(0) as u64,
            lead: 0,
        })
    }
}

fn plan(name: &str) -> Plan {
    let path = format!(
        "{}/../../schemas/examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    resolve_fonts(&mut plan);
    plan.resolve_paths(&gmclip_model::lab::root());
    plan
}

/// An example plan's font paths are as written (`fonts/…` in bench-60-m0): relative to
/// the lab media folder, where `tools/lab_media.sh` links the face.
fn resolve_fonts(plan: &mut Plan) {
    for f in plan.fonts.iter_mut().flatten() {
        if std::path::Path::new(&f.path).is_relative() {
            f.path = gmclip_model::lab::media_str(&f.path);
        }
    }
}

/// A golden cut's frozen RenderPlan (`lab/fixtures/golden-11`, written once from the
/// cut's own spec), its paths resolved against the lab media folder.
fn golden(name: &str) -> Plan {
    let p = gmclip_model::lab::fixture(format!("golden-11/{name}.renderplan.json"));
    let mut plan = gmclip_plan::parse(&std::fs::read(&p).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    plan
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn h79_parts_1_3_render() {
    let plan = plan("h79-parts-1-3.renderplan.json");
    let out = lab_dir().join("h10-h79-parts-1-3.mp4");
    let mut inputs = ShimInputs::default();
    let rep = export(&plan, &mut inputs, &out, &ExportOptions::default()).unwrap();
    eprintln!(
        "H-10 h79 parts 1–3: {} frames in {:.2} s ({:.0} fps); peak {:.2} Mbps (rung {}); {} video seeks; notes {:?}",
        rep.frames,
        rep.seconds,
        rep.frames as f64 / rep.seconds,
        rep.video_peak_bps as f64 / 1e6,
        rep.rate_rung,
        rep.video_seeks,
        rep.notes
    );
    let info = String::from_utf8(run(Command::new(FFPROBE)
        .args(["-v", "error", "-count_packets", "-show_entries",
            "stream=codec_name,profile,level,width,height,has_b_frames,nb_read_packets,start_pts,duration_ts,time_base,sample_rate,color_space,color_range",
            "-of", "compact"])
        .arg(&out)))
    .unwrap();
    eprintln!("{info}");
    assert!(info.contains("codec_name=h264|profile=High") && info.contains("level=40"));
    assert!(info.contains("width=1080|height=1920"));
    assert!(info.contains("nb_read_packets=397"), "397 video samples");
    assert!(info.contains("codec_name=aac"));
    // Clean decode of everything.
    let dec = Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        dec.status.success() && dec.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&dec.stderr)
    );
    // The export record sits beside it.
    let rec = std::fs::read_to_string(out.with_extension("export.json")).unwrap();
    assert!(
        rec.contains("\"render_sig\"") && rec.contains("\"sha256\""),
        "{rec}"
    );
}

fn ssim_y(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let (mut sum, mut n) = (0.0, 0usize);
    for y in (0..h - 8).step_by(4) {
        for x in (0..w - 8).step_by(4) {
            let (mut ma, mut mb, mut va, mut vb, mut cov) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for j in 0..8 {
                for i in 0..8 {
                    let p = (y + j) * w + x + i;
                    let (pa, pb) = (f64::from(a[p]), f64::from(b[p]));
                    ma += pa;
                    mb += pb;
                    va += pa * pa;
                    vb += pb * pb;
                    cov += pa * pb;
                }
            }
            let (ma, mb) = (ma / 64.0, mb / 64.0);
            let (va, vb, cov) = (
                va / 64.0 - ma * ma,
                vb / 64.0 - mb * mb,
                cov / 64.0 - ma * mb,
            );
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1;
        }
    }
    sum / n as f64
}

/// The rendered pictures are the plan's: output frames from each part, decoded from
/// the MP4, against the CPU compositor on the same source frames (ffmpeg-decoded);
/// and the audio is the plan's mix at lag 0 (xcorr over three windows).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn h79_render_content_and_av_alignment() {
    use gmclip_media::audio::graph::{AudioPlan, PcmSource};
    use gmclip_media::backend::vt::AacRandomReader;
    use gmclip_render::compositor::cpu::{FrameProvider, Nv12Image, compose};
    use gmclip_render::compositor::{ComposeError, frame_draw};
    use gmclip_render::nv12::rgb_to_nv12_cpu;
    use std::sync::Arc;

    let plan = plan("h79-parts-1-3.renderplan.json");
    let out = lab_dir().join("h10-h79-content.mp4");
    let mut inputs = ShimInputs::default();
    export(&plan, &mut inputs, &out, &ExportOptions::default()).unwrap();
    let (w, h) = (1080usize, 1920usize);
    let mkv = path_of(&plan, "mkv1440").to_owned();
    struct One(Arc<Nv12Image>);
    impl FrameProvider for One {
        fn frame(&mut self, _: &str, _: u64) -> Result<Arc<Nv12Image>, ComposeError> {
            Ok(self.0.clone())
        }
    }
    let mut worst: f64 = 1.0;
    // the export draws the plan's text over the picture (H-06, 5baf6f2): so does the
    // reference, from the same tiles
    let mut text = gmclip_render::compositor::text_layer::TextLayer::new(&plan).unwrap();
    for f in [5u64, 30, 54, 60, 120, 175, 180, 300, 396] {
        let rendered = run(Command::new(FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args([
                "-vf",
                &format!("select=eq(n\\,{f})"),
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-",
            ]));
        let draw = frame_draw(&plan, f).unwrap();
        let idx = draw.layers[0].frame;
        let src = run(Command::new(FFMPEG).args([
            "-v",
            "error",
            "-ss",
            &format!("{:.6}", idx as f64 / 30.0),
            "-i",
            &mkv,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ]));
        let (sw, sh) = (2560usize, 1440usize);
        let (u, v) = (&src[sw * sh..sw * sh * 5 / 4], &src[sw * sh * 5 / 4..]);
        let uv: Vec<u8> = u.iter().zip(v).flat_map(|(a, b)| [*a, *b]).collect();
        let img = Nv12Image {
            width: sw as u32,
            height: sh as u32,
            y: src[..sw * sh].to_vec(),
            uv,
            siting: ChromaSiting::Center,
        };
        let mut canvas = compose(&draw, w as u32, h as u32, &mut One(Arc::new(img))).unwrap();
        let tiles = text.active(&plan, f).unwrap();
        gmclip_render::compositor::text_layer::composite_cpu(&mut canvas, &tiles);
        let (ry, _) = rgb_to_nv12_cpu(&canvas.rgb(), w, h);
        let s = ssim_y(&rendered[..w * h], &ry, w, h);
        eprintln!("H-10 content: out frame {f} (source {idx}): SSIM vs CPU composite {s:.4}");
        worst = worst.min(s);
    }
    assert!(
        worst >= 0.97,
        "rendered frames are not the plan's pictures ({worst:.4})"
    );

    // Audio: the planned mix (before loudness) vs the decoded export, lag 0.
    let ap = AudioPlan::from_plan(&plan).unwrap();
    struct Src(AacRandomReader<Box<dyn SampleSource + Send>>);
    impl PcmSource for Src {
        fn read(&mut self, _: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
            self.0.read(first, out).map_err(|e| e.to_string())
        }
    }
    let a = inputs.audio(&plan, "proxy").unwrap();
    let mut src = Src(AacRandomReader::new(a.source, &a.asc, 48_000, 2, a.skip).unwrap());
    let mix = ap.mix(&mut src).unwrap();
    let dec = run(Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-map", "0:a:0", "-f", "f32le", "-ac", "2", "-"]));
    let got: Vec<f32> = dec
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let mono = |x: &[f32]| -> Vec<f32> { x.chunks_exact(2).map(|c| c[0] + c[1]).collect() };
    let (m, g) = (mono(&mix), mono(&got));
    eprintln!(
        "H-10 audio: planned {} samples, decoded {} samples",
        m.len(),
        g.len()
    );
    assert!(
        g.len() >= m.len() && g.len() < m.len() + 1024,
        "audio length"
    );
    for start in [30_000usize, 200_000, 500_000] {
        let len = 48_000;
        let mut best = (f64::MIN, 0i64);
        for lag in -960i64..=960 {
            let mut acc = 0f64;
            for (i, &mv) in m.iter().enumerate().skip(start).take(len) {
                let j = i as i64 + lag;
                if j >= 0 && (j as usize) < g.len() {
                    acc += f64::from(mv) * f64::from(g[j as usize]);
                }
            }
            if acc > best.0 {
                best = (acc, lag);
            }
        }
        eprintln!("H-10 audio xcorr at sample {start}: lag {}", best.1);
        assert_eq!(best.1, 0, "A/V offset at {start}");
    }
}

/// BENCH-60-M0's sources (plan id, lab media path): (a), the 1440p H.264 MMO restream;
/// (b), the 1440p VP9 MKV; and (b)'s 48 kHz audio member.
const BENCH_SOURCES: [(&str, &str); 3] = [
    (
        "a",
        "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
    ),
    ("b-mkv", "streams/jMcAmooE53I-source.mkv"),
    ("b-proxy", "preview/loc_4fadb756.proxy.mp4"),
];

/// BENCH-60-M0 (60 s, 41 parts alternating the 1440p H.264 restream and the 1440p VP9
/// MKV, 4 layers) — the R-EXP-1 workload — with the example plan's sources pointed at
/// AJ's files. Reports wall time; the ≤ 30 s gate is measured on mac-ref (H-15).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn bench_60_m0_render() {
    // R-EXP-1's workload is the whole 41-part plan: without one of its sources it is
    // not measured at all.
    if gmclip_model::lab::skip_without(BENCH_SOURCES.map(|(_, rel)| rel)) {
        return;
    }
    let mut plan = plan("bench-60-m0.renderplan.json");
    for s in &mut plan.sources {
        s.path = BENCH_SOURCES
            .iter()
            .find(|(id, _)| *id == s.id.as_str())
            .map(|(_, rel)| gmclip_model::lab::media_str(rel))
            .unwrap_or_else(|| panic!("source {}", s.id.as_str()));
        s.fingerprint = None;
    }
    let out = lab_dir().join("h10-bench-60-m0.mp4");
    let mut inputs = ShimInputs::default();
    // Probing (the shim's ffprobe pass) is not part of the render: warm it first.
    for id in ["a", "b-mkv"] {
        let _ = inputs.video(&plan, id).unwrap();
    }
    for id in ["a", "b-proxy"] {
        let _ = inputs.audio(&plan, id).unwrap();
    }
    let rep = export(&plan, &mut inputs, &out, &ExportOptions::default()).unwrap();
    eprintln!(
        "H-10 BENCH-60-M0: {} frames in {:.2} s ({:.0} fps); peak {:.2} Mbps (rung {}); {} video seeks; {}",
        rep.frames,
        rep.seconds,
        rep.frames as f64 / rep.seconds,
        rep.video_peak_bps as f64 / 1e6,
        rep.rate_rung,
        rep.video_seeks,
        String::from_utf8(run(&mut Command::new("/usr/bin/uptime")))
            .unwrap()
            .trim()
    );
    let info = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-count_packets",
            "-show_entries",
            "stream=codec_name,nb_read_packets,duration_ts",
            "-of",
            "compact",
        ])
        .arg(&out)))
    .unwrap();
    eprintln!("{info}");
    assert!(info.contains("nb_read_packets=1800"));
    let dec = Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        dec.status.success() && dec.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&dec.stderr)
    );
}

/// H-10 acceptance: the 11 golden cuts (their frozen RenderPlans, ASS cues as imported)
/// and rendered at the Reels preset: frames = plan, both tracks complete,
/// clean decode. Text is not drawn yet (H-06). Outputs are deleted after the check.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn eleven_golden_cuts_render() {
    let set = common::goldens_here(|_| Vec::new());
    if set.skip_if_empty() {
        return;
    }
    let mut total = 0.0;
    for name in &set.items {
        let plan = golden(name);
        let out = lab_dir().join(format!("h10-{name}.mp4"));
        let mut inputs = ShimInputs::default();
        for s in &plan.sources {
            let id = s.id.as_str();
            if plan.parts.iter().any(|p| p.picture.source.as_str() == id) {
                let _ = inputs.video(&plan, id).unwrap();
            }
            if plan.parts.iter().any(|p| p.audio.source.as_str() == id) {
                let _ = inputs.audio(&plan, id).unwrap();
            }
        }
        let rep = export(&plan, &mut inputs, &out, &ExportOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        total += rep.seconds;
        let info = String::from_utf8(run(Command::new(FFPROBE)
            .args([
                "-v",
                "error",
                "-count_packets",
                "-show_entries",
                "stream=codec_name,nb_read_packets,duration_ts",
                "-of",
                "csv=p=0",
            ])
            .arg(&out)))
        .unwrap();
        let lines: Vec<&str> = info.lines().collect();
        let dec = Command::new(FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        eprintln!(
            "H-10 {name}: {} frames, {} parts in {:.2} s; peak {:.2} Mbps (rung {}); {:?}",
            plan.output.frames,
            plan.parts.len(),
            rep.seconds,
            rep.video_peak_bps as f64 / 1e6,
            rep.rate_rung,
            lines
        );
        // Video: one sample per frame lasting the plan (512 ticks per frame at 30 fps);
        // audio: exactly S(frames) samples (1600 per frame).
        assert_eq!(
            lines[0],
            format!("h264,{},{}", plan.output.frames * 512, plan.output.frames),
            "{name}: video"
        );
        assert!(
            lines[1].starts_with(&format!("aac,{},", plan.output.frames * 1600)),
            "{name}: audio {lines:?}"
        );
        assert!(
            dec.status.success() && dec.stderr.is_empty(),
            "{name}: {}",
            String::from_utf8_lossy(&dec.stderr)
        );
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(out.with_extension("export.json"));
    }
    eprintln!(
        "H-10: {} rendered in {total:.1} s total",
        set.note("golden cuts")
    );
}

/// Two renders of one plan: identical decoded pictures and identical PCM.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn render_twice_is_deterministic() {
    // Decision 0005: identical frames before encode (frames_hash) and PCM; decoded
    // frames SSIM ≥ 0.995 per frame after (VideoToolbox is not bit-reproducible).
    let plan = plan("h79-parts-1-3.renderplan.json");
    let mut runs = Vec::new();
    let outs: Vec<PathBuf> = (0..2)
        .map(|k| lab_dir().join(format!("h10-det-{k}.mp4")))
        .collect();
    for out in &outs {
        let opts = ExportOptions {
            hash_encoder_input: true,
            ..Default::default()
        };
        let rep = export(&plan, &mut ShimInputs::default(), out, &opts).unwrap();
        let rec = std::fs::read_to_string(out.with_extension("export.json")).unwrap();
        let a = run(Command::new(FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(out)
            .args(["-map", "0:a:0", "-f", "md5", "-"]));
        runs.push((rep.encoder_input_hashes, String::from_utf8(a).unwrap(), rec));
    }
    let input_diff = runs[0]
        .0
        .iter()
        .zip(&runs[1].0)
        .filter(|(a, b)| a != b)
        .count();
    let stats = lab_dir().join("h10-det-ssim.txt");
    run(Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(&outs[0])
        .arg("-i")
        .arg(&outs[1])
        .args([
            "-lavfi",
            &format!("ssim=stats_file={}", stats.display()),
            "-f",
            "null",
            "-",
        ]));
    let text = std::fs::read_to_string(&stats).unwrap();
    let ssims: Vec<f64> = text
        .lines()
        .filter_map(|l| {
            l.split("All:")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .collect();
    let min = ssims.iter().copied().fold(1.0f64, f64::min);
    eprintln!(
        "H-10 determinism: encoder input {input_diff} of {} frames differ; decoded per-frame SSIM min {min:.5} over {} frames; PCM {}",
        runs[0].0.len(),
        ssims.len(),
        if runs[0].1 == runs[1].1 {
            "identical"
        } else {
            "differs"
        }
    );
    for out in &outs {
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(out.with_extension("export.json"));
    }
    let _ = std::fs::remove_file(&stats);
    assert_eq!(runs[0].0.len(), plan.output.frames as usize);
    assert_eq!(
        input_diff, 0,
        "the pictures sent to the encoder differ between renders"
    );
    assert!(
        runs[0].2.contains("\"frames_hash\""),
        "frames_hash recorded"
    );
    assert_eq!(ssims.len(), plan.output.frames as usize);
    assert!(min >= 0.995, "decoded SSIM {min}");
    assert_eq!(runs[0].1, runs[1].1, "PCM differs between renders");
}

/// A cancelled export fails with step `cancelled` and installs nothing.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn cancelled_export_installs_nothing() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let plan = plan("h79-parts-1-3.renderplan.json");
    let out = lab_dir().join("h10-cancelled.mp4");
    let _ = std::fs::remove_file(&out);
    let opts = ExportOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    let e = export(&plan, &mut ShimInputs::default(), &out, &opts).unwrap_err();
    assert!(
        matches!(
            e,
            RenderError::Failed {
                step: "cancelled",
                ..
            }
        ),
        "{e}"
    );
    assert!(!out.exists());
}

/// H-05b (K3 early warning): for each of the 11 golden cuts, 16 output frames rendered
/// exactly as the export renders them, beside RT's approved render at the same frame,
/// IG overlay zones tinted on ours — one PNG sheet per cut under
/// `target/lab-media/h05b/`. Text is not drawn yet (H-06), so captions/hooks/pills
/// appear only on RT's side.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn h05b_still_sheets() {
    use gmclip_render::export::render_frames;
    use gmclip_render::export::sheet::{RgbImage, SheetCell, contact_sheet, write_png};
    let set = common::goldens_here(|n| vec![common::golden_mp4(n)]);
    if set.skip_if_empty() {
        return;
    }
    eprintln!("H-05b on {}", set.note("golden cuts"));
    let dir = lab_dir().join("h05b");
    std::fs::create_dir_all(&dir).unwrap();
    for name in &set.items {
        let rt_mp4 = common::golden_mp4(name);
        let plan = golden(name);
        let n = plan.output.frames;
        let at: Vec<u64> = (0..16).map(|k| k * (n - 1) / 15).collect();
        let ours = render_frames(&plan, &mut ShimInputs::default(), &at)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut cells = Vec::new();
        for (f, img) in ours {
            let raw = run(Command::new(FFMPEG)
                .args(["-v", "error", "-i"])
                .arg(&rt_mp4)
                .args([
                    "-vf",
                    &format!("select=eq(n\\,{f}),scale=in_color_matrix=bt709:in_range=tv"),
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "-",
                ]));
            let ok = raw.len() == 1080 * 1920 * 3;
            let reference = ok.then_some(RgbImage {
                width: 1080,
                height: 1920,
                px: raw,
            });
            cells.push(SheetCell {
                frame: f,
                ours: img,
                reference,
            });
        }
        let sheet = contact_sheet(&cells, 4, 4);
        let path = dir.join(format!("{name}.png"));
        write_png(&sheet, &path).unwrap();
        // Our writer stores (no deflate); recompress for AJ's phone (dev tool only).
        let small = dir.join(format!("{name}.small.png"));
        run(Command::new(FFMPEG)
            .args(["-v", "error", "-y", "-i"])
            .arg(&path)
            .arg(&small));
        std::fs::rename(&small, &path).unwrap();
        let probe = run(Command::new(FFPROBE)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path));
        eprintln!(
            "H-05b {name}: {} frames sampled → {} ({})",
            at.len(),
            path.display(),
            String::from_utf8_lossy(&probe).trim()
        );
        assert!(
            cells.iter().all(|c| c.reference.is_some()),
            "{name}: RT frame missing"
        );
    }
}

/// Decode + composite + NV12 output are deterministic on their own (the encoder input
/// is identical between runs), checked on 60 consecutive output frames twice.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn encoder_input_is_deterministic() {
    use gmclip_render::export::render_frames;
    let plan = plan("h79-parts-1-3.renderplan.json");
    let at: Vec<u64> = (40..100).collect();
    let a = render_frames(&plan, &mut ShimInputs::default(), &at).unwrap();
    let b = render_frames(&plan, &mut ShimInputs::default(), &at).unwrap();
    let diff = a.iter().zip(&b).filter(|(x, y)| x.1 != y.1).count();
    eprintln!(
        "H-10 encoder input: {diff} of {} frames differ between runs",
        at.len()
    );
    assert_eq!(diff, 0);
}

/// Cancelling mid-render (from another thread, during the video pass) fails with step
/// `cancelled`, installs nothing and leaves no partial file.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media (run with --features lab)"
)]
fn cancel_mid_render_cleans_up() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let plan = plan("h79-parts-1-3.renderplan.json");
    let out = lab_dir().join("h10-cancel-mid.mp4");
    let _ = std::fs::remove_file(&out);
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let partial = out.with_extension("partial.mp4");
    let p2 = partial.clone();
    // Cancel once the partial file exists (the video pass has started writing).
    let t = std::thread::spawn(move || {
        for _ in 0..600 {
            if p2.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        f2.store(true, Ordering::Relaxed);
    });
    let opts = ExportOptions {
        cancel: Some(flag),
        ..Default::default()
    };
    let e = export(&plan, &mut ShimInputs::default(), &out, &opts).unwrap_err();
    t.join().unwrap();
    assert!(
        matches!(
            e,
            RenderError::Failed {
                step: "cancelled",
                frame: Some(_),
                ..
            }
        ),
        "{e}"
    );
    assert!(!out.exists(), "nothing installed");
    assert!(!partial.exists(), "partial removed");
}

/// Gaussian-window SSIM (11 taps, σ 1.5, S4's method) on luma over a rect.
fn ssim_gauss(a: &[f64], b: &[f64], w: usize, r: [usize; 4]) -> f64 {
    let [x0, y0, x1, y1] = r;
    let k: Vec<f64> = (-5i32..=5)
        .map(|i| (-f64::from(i * i) / (2.0 * 1.5 * 1.5)).exp())
        .collect();
    let ks: f64 = k.iter().sum();
    let k: Vec<f64> = k.iter().map(|v| v / ks).collect();
    let (rw, rh) = (x1 - x0, y1 - y0);
    let blur = |f: &dyn Fn(usize) -> f64| -> Vec<f64> {
        let mut h = vec![0.0; rw * rh];
        for y in 0..rh {
            for x in 0..rw {
                let mut acc = 0.0;
                for (t, kv) in k.iter().enumerate() {
                    let xx = (x as i64 + t as i64 - 5).clamp(0, rw as i64 - 1) as usize;
                    acc += kv * f((y0 + y) * w + x0 + xx);
                }
                h[y * rw + x] = acc;
            }
        }
        let mut v = vec![0.0; rw * rh];
        for y in 0..rh {
            for x in 0..rw {
                let mut acc = 0.0;
                for (t, kv) in k.iter().enumerate() {
                    let yy = (y as i64 + t as i64 - 5).clamp(0, rh as i64 - 1) as usize;
                    acc += kv * h[yy * rw + x];
                }
                v[y * rw + x] = acc;
            }
        }
        v
    };
    let ma = blur(&|i| a[i]);
    let mb = blur(&|i| b[i]);
    let saa = blur(&|i| a[i] * a[i]);
    let sbb = blur(&|i| b[i] * b[i]);
    let sab = blur(&|i| a[i] * b[i]);
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let mut sum = 0.0;
    for i in 0..rw * rh {
        let (va, vb, cv) = (
            saa[i] - ma[i] * ma[i],
            sbb[i] - mb[i] * mb[i],
            sab[i] - ma[i] * mb[i],
        );
        sum += ((2.0 * ma[i] * mb[i] + c1) * (2.0 * cv + c2))
            / ((ma[i] * ma[i] + mb[i] * mb[i] + c1) * (va + vb + c2));
    }
    sum / (rw * rh) as f64
}

/// H-06 (acceptance: per-frame SSIM ≥ 0.98 vs libass-burned references, 16 frames per
/// cut, Neometric). For each frame gmclip renders the cut twice through the real export
/// path (demuxers → VT decode → GPU compositor): with its text, and without. The
/// reference is RT's own libass (`static_ffmpeg`, the build RT burned with) burning the
/// cut's ASS onto the text-less frame at the frame's pts, so the comparison isolates the
/// text (shaping, raster, placement per `out`/layer, premultiplied over) from codec and
/// picture differences. Gate per frame: Gaussian SSIM on luma over each drawn tile
/// (+8 px) ≥ 0.98; the tile's registration against libass (a Lucas–Kanade step,
/// checked on a known 0.25-px text-only shift) within 0.2 px on each axis (S4: SSIM
/// alone passes a quarter-pixel shift); and nothing libass drew outside our tiles. Also printed, ungated: the same SSIM against
/// RT's encoded MP4 (adds x264's edge loss and picture differences).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + AJ's media, fonts and static_ffmpeg (run with --features lab)"
)]
fn h06_text_vs_rt_libass() {
    use gmclip_render::compositor::text_layer::TextLayer;
    use gmclip_render::export::sheet::write_png;
    use gmclip_render::export::{DemuxInputs, render_frames};
    let libass = std::env::var("GMCLIP_LIBASS_FFMPEG").unwrap_or_else(|_| "static_ffmpeg".into());
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tmp = repo.join("target/lab-media/h06");
    std::fs::create_dir_all(&tmp).unwrap();
    let set = common::goldens_here(|n| vec![common::golden_ass_of(n), common::golden_mp4(n)]);
    if set.skip_if_empty() {
        return;
    }
    let luma = |rgb: &[u8]| -> Vec<f64> {
        rgb.chunks_exact(3)
            .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
            .collect()
    };
    // Registration: one symmetric Lucas–Kanade step, the (dx, dy) by which `a` is `b`
    // moved, over the rect (central-difference gradients of the mean image). Threshold-
    // free and unbiased toward whole pixels (the review measured a 1/8-px bilinear
    // search reading a true 0.375 px as 0.25); it reads sub-pixel shifts ≈ 5–20 % high,
    // which errs toward failing.
    let shift = |a: &[f64], b: &[f64], r: [usize; 4]| -> [f64; 2] {
        let (mut m, mut v) = ([0.0f64; 3], [0.0f64; 2]);
        let s = |i: usize| 0.5 * (a[i] + b[i]);
        for y in r[1] + 1..r[3] - 1 {
            for x in r[0] + 1..r[2] - 1 {
                let i = y * 1080 + x;
                let gx = 0.5 * (s(i + 1) - s(i - 1));
                let gy = 0.5 * (s(i + 1080) - s(i - 1080));
                let e = a[i] - b[i];
                m[0] += gx * gx;
                m[1] += gx * gy;
                m[2] += gy * gy;
                v[0] -= gx * e;
                v[1] -= gy * e;
            }
        }
        let det = m[0] * m[2] - m[1] * m[1];
        if det.abs() < 1e-9 {
            return [0.0, 0.0];
        }
        [
            (m[2] * v[0] - m[1] * v[1]) / det,
            (m[0] * v[1] - m[1] * v[0]) / det,
        ]
    };
    // A tile composited (premultiplied over) onto a luma-only copy of an RGB frame.
    let over = |bg: &gmclip_render::export::sheet::RgbImage,
                t: &gmclip_render::text::raster::Tile| {
        let mut px = bg.px.clone();
        for ty in 0..t.height as i64 {
            for tx in 0..t.width as i64 {
                let (x, y) = (i64::from(t.x) + tx, i64::from(t.y) + ty);
                if !(0..1080).contains(&x) || !(0..1920).contains(&y) {
                    continue;
                }
                let s = ((ty * i64::from(t.width) + tx) * 4) as usize;
                let a = f64::from(t.rgba[s + 3]) / 255.0;
                let d = ((y * 1080 + x) * 3) as usize;
                for k in 0..3 {
                    let v = f64::from(t.rgba[s + k]) + f64::from(px[d + k]) * (1.0 - a);
                    px[d + k] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        luma(&px)
    };
    let (mut all, mut all_rt, mut worst_off) = (Vec::new(), Vec::new(), 0.0f64);
    let mut bbox_fail: Vec<String> = Vec::new();
    let mut probed = false;
    for name in &set.items {
        let (ass, rt_mp4) = (common::golden_ass_of(name), common::golden_mp4(name));
        let plan = golden(name);
        let mut bare = plan.clone();
        bare.text_events = None;
        let n = plan.output.frames;
        let at: Vec<u64> = (0..16).map(|k| k * (n - 1) / 15).collect();
        let ours = render_frames(&plan, &mut DemuxInputs::new(None), &at)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let bgs = render_frames(&bare, &mut DemuxInputs::new(None), &at)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut text = TextLayer::new(&plan).unwrap();
        let (mut worst, mut worst_rt, mut with_text) = (1.0f64, 1.0f64, 0);
        for ((f, img), (_, bg)) in ours.iter().zip(&bgs) {
            let tiles = text.active(&plan, *f).unwrap();
            let bg_png = tmp.join(format!("{name}-bg.png"));
            write_png(bg, &bg_png).unwrap();
            // libass at the frame's pts (time base 1/30, pts = f): RT's own time rule.
            let reference = run(Command::new(&libass)
                .args(["-v", "error", "-framerate", "30", "-i"])
                .arg(&bg_png)
                .args([
                    "-vf",
                    &format!("setpts={f},ass={}", ass.display()),
                    "-vsync",
                    "0",
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "-",
                ]));
            let (yo, yb, yr) = (luma(&img.px), luma(&bg.px), luma(&reference));
            if tiles.is_empty() {
                // Nothing drawn: libass must draw nothing either.
                let max = yr
                    .iter()
                    .zip(&yb)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f64, f64::max);
                assert!(
                    max <= 1.0,
                    "{name} frame {f}: libass drew text where the plan has none (max Δ {max})"
                );
                continue;
            }
            let rt = run(Command::new(FFMPEG)
                .args(["-v", "error", "-i"])
                .arg(&rt_mp4)
                .args([
                    "-vf",
                    &format!("select=eq(n\\,{f}),scale=in_color_matrix=bt709:in_range=tv"),
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "-",
                ]));
            let yt = luma(&rt);
            let (mut m, mut m_rt, mut dump) = (1.0f64, 1.0f64, false);
            // Coverage: everything libass drew lies inside our tiles (+8 px), so text we
            // left out is caught even on a frame where we drew something else.
            let inside = |i: usize| {
                let (x, y) = ((i % 1080) as i32, (i / 1080) as i32);
                tiles.iter().any(|t| {
                    x >= t.x - 8
                        && y >= t.y - 8
                        && x < t.x + t.width as i32 + 8
                        && y < t.y + t.height as i32 + 8
                })
            };
            let stray = (0..yr.len())
                .filter(|&i| (yr[i] - yb[i]).abs() > 2.0 && !inside(i))
                .count();
            if stray > 0 {
                dump = true;
                bbox_fail.push(format!(
                    "{name} frame {f}: libass drew {stray} px outside every gmclip tile"
                ));
            }
            if !probed {
                // The detector on a known text-only shift: the first active event drawn
                // again by our raster at pos + (0.25, 0.25) px, over the same background.
                probed = true;
                let ev = plan
                    .text_events
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|e| e.out.start() <= *f && *f < e.out.end())
                    .unwrap();
                let style = &plan.text_styles.as_ref().unwrap()[&ev.style];
                let mut raster = gmclip_render::text::raster::TextRaster::new();
                for fo in plan.fonts.iter().flatten() {
                    let face =
                        gmclip_render::text::font::FontFace::open(std::path::Path::new(&fo.path))
                            .unwrap();
                    raster.add_font(fo.id.as_str(), face).unwrap();
                }
                let t0 = raster.render(ev, style).unwrap();
                let mut moved = ev.clone();
                moved.pos.x += 0.25;
                moved.pos.y += 0.25;
                let t1 = raster.render(&moved, style).unwrap();
                let r = [
                    (t0.x.min(t1.x) - 8).max(1) as usize,
                    (t0.y.min(t1.y) - 8).max(1) as usize,
                    ((t0.x + t0.width as i32).max(t1.x + t1.width as i32) + 8).min(1079) as usize,
                    ((t0.y + t0.height as i32).max(t1.y + t1.height as i32) + 8).min(1919) as usize,
                ];
                let got = shift(&over(bg, &t1), &over(bg, &t0), r);
                eprintln!(
                    "H-06 probe: text moved (0.25, 0.25) px reads ({:.3}, {:.3})",
                    got[0], got[1]
                );
                assert!(
                    got.iter().all(|g| (0.2..=0.4).contains(g)),
                    "{name} frame {f}: registration probe read {got:?} for (0.25, 0.25)"
                );
            }
            let mut measured = 0;
            for t in &tiles {
                let r = [
                    (t.x - 8).max(0) as usize,
                    (t.y - 8).max(0) as usize,
                    ((t.x + t.width as i32 + 8).max(0) as usize).min(1080),
                    ((t.y + t.height as i32 + 8).max(0) as usize).min(1920),
                ];
                if r[2] <= r[0] + 11 || r[3] <= r[1] + 11 {
                    continue;
                }
                measured += 1;
                m = m.min(ssim_gauss(&yo, &yr, 1080, r));
                m_rt = m_rt.min(ssim_gauss(&yo, &yt, 1080, r));
                if r[2] > r[0] + 16
                    && r[3] > r[1] + 16
                    && r[0] >= 1
                    && r[1] >= 1
                    && r[2] < 1080
                    && r[3] < 1920
                {
                    let [dx, dy] = shift(&yo, &yr, r);
                    let off = dx.abs().max(dy.abs());
                    worst_off = worst_off.max(off);
                    if off > 0.2 {
                        dump = true;
                        bbox_fail.push(format!(
                            "{name} frame {f}: tile at ({}, {}) is offset ({dx}, {dy}) px from libass",
                            t.x, t.y
                        ));
                    }
                }
            }
            if m < 0.98 || dump {
                write_png(img, &tmp.join(format!("{name}-{f}-ours.png"))).unwrap();
                let r = gmclip_render::export::sheet::RgbImage {
                    width: 1080,
                    height: 1920,
                    px: reference,
                };
                write_png(&r, &tmp.join(format!("{name}-{f}-libass.png"))).unwrap();
            }
            assert!(
                measured > 0,
                "{name} frame {f}: no tile large enough to measure"
            );
            worst = worst.min(m);
            worst_rt = worst_rt.min(m_rt);
            with_text += 1;
            all.push(m);
            all_rt.push(m_rt);
        }
        let _ = std::fs::remove_file(tmp.join(format!("{name}-bg.png")));
        eprintln!(
            "H-06 {name}: {with_text}/16 frames with text; SSIM vs libass-burned min {worst:.4}; vs RT's MP4 min {worst_rt:.4}"
        );
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (v[0], v[v.len() / 2])
    };
    let ((min, med), (min_rt, med_rt)) = (stats(&mut all), stats(&mut all_rt));
    let below = all.iter().filter(|&&s| s < 0.98).count();
    eprintln!("H-06 on {}", set.note("golden cuts"));
    eprintln!(
        "H-06: {} frames with text; SSIM vs libass-burned min {min:.4} median {med:.4} ({below} < 0.98); registration offset max {worst_off:.3} px; vs RT's MP4 (ungated) min {min_rt:.4} median {med_rt:.4}",
        all.len()
    );
    assert!(
        bbox_fail.is_empty(),
        "text offsets over 0.2 px: {bbox_fail:#?}"
    );
    assert_eq!(below, 0, "frames under 0.98 (dumped to {})", tmp.display());
}

/// Decision 0027: YouTube 1440p60 at its full 36 Mbit/s (peak 54) on H.264 High 5.1.
/// A 4 s 2560×1440 60 fps source with moving detail and grain (ffmpeg-made, so it needs
/// more than 36 Mbit/s) is exported through the plan the 1440p60 preset writes; the file is
/// High@5.1, its average video rate lands in the target band (0.8–1.15 × 36 Mbit/s, far
/// above the old 25 Mbit/s cap) and its 1-s peak under the plan's 54.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal and ffmpeg (run with --features lab)"
)]
fn youtube_1440p60_reaches_its_rate() {
    let dir = lab_dir().join("k3-1440p60");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("busy-1440p60.mp4");
    if !src.is_file() {
        run(Command::new(FFMPEG)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=s=2560x1440:r=60:d=4,noise=alls=24:allf=t+u,format=yuv420p")
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=4",
            ])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "12"])
            .args(["-c:a", "aac", "-ac", "2", "-b:a", "128k", "-shortest"])
            .arg(&src));
    }
    let frames = 240u64;
    let v = serde_json::json!({
        "schema": "gmclip.renderplan/1",
        "output": {"canvas": {"w": 2560, "h": 1440}, "fps": {"num": 60, "den": 1}, "frames": frames,
            "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
            "video": {"codec": "h264", "profile": "high", "level": "5.1", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                      "rate": {"mode": "vbr", "target_bps": 36_000_000u64, "max_bps": 54_000_000u64}, "timescale": 30720},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
            "container": {"moov_first": true, "edit_lists": false}},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "s1", "path": src.display().to_string()}],
        "parts": [{"id": "p1", "out": [0, frames],
            "picture": {"source": "s1", "first_frame": 0},
            "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
            "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": 2560, "h": 1440},
                        "place": {"x": 0, "y": 0, "w": 2560, "h": 1440}, "z": 0}]}],
        "audio": {"loudness": {"mode": "as_recorded"}},
    });
    let plan: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let out = dir.join("youtube-1440p60.mp4");
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let rep = export(&plan, &mut inputs, &out, &ExportOptions::default()).unwrap();
    let info = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=profile,level,width,height,r_frame_rate,nb_frames,bit_rate",
            "-of",
            "compact",
        ])
        .arg(&out)))
    .unwrap();
    let bps: f64 = info
        .split('|')
        .find_map(|kv| kv.trim().strip_prefix("bit_rate="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap();
    eprintln!(
        "1440p60: {info} — {:.2} Mbit/s average, peak {:.2} (rung {}), {:.1} s; notes {:?}",
        bps / 1e6,
        rep.video_peak_bps as f64 / 1e6,
        rep.rate_rung,
        rep.seconds,
        rep.notes
    );
    assert!(
        info.contains("profile=High") && info.contains("level=51"),
        "{info}"
    );
    assert!(info.contains("width=2560|height=1440") && info.contains("r_frame_rate=60/1"));
    assert_eq!(rep.frames, frames);
    assert_eq!(rep.rate_rung, 0, "no retry: the cap is the level's");
    assert!(
        (0.8 * 36e6..=1.15 * 36e6).contains(&bps),
        "average {:.2} Mbit/s outside the 36 Mbit/s band",
        bps / 1e6
    );
    assert!(
        rep.video_peak_bps <= 54_000_000 * 11 / 10,
        "peak {:.2} Mbit/s",
        rep.video_peak_bps as f64 / 1e6
    );
    assert!(rep.notes.iter().all(|n| !n.starts_with("encoder:")));
}
