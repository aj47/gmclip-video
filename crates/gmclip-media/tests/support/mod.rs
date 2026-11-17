//! Lab-test support (dev-only, never shipped): a temporary ffprobe-driven sample
//! source standing in for Lane B's demuxers (B-01/B-02), ffmpeg-made references
//! (dev tools MAY shell out to ffmpeg; SPEC §3.1 `tools/`), MD5, and a seeded PRNG.
#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use gmclip_media::backend::SampleSource;
use gmclip_media::{MediaError, SampleIndex, index::SampleIdx};

pub const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
pub const FFPROBE: &str = "/opt/homebrew/bin/ffprobe";

/// AJ's real sources (read-only).
/// `streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4` in the lab media folder.
pub fn mmo_1440p() -> &'static str {
    static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        gmclip_model::lab::media_str(
            "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
        )
    });
    P.as_str()
}
/// `preview/loc_4fadb756.proxy.mp4` in the lab media folder.
pub fn proxy_4fadb756() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("preview/loc_4fadb756.proxy.mp4"));
    P.as_str()
}
/// `preview/loc_bd89f38b.proxy.mp4` in the lab media folder.
pub fn proxy_bd89f38b() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("preview/loc_bd89f38b.proxy.mp4"));
    P.as_str()
}
/// `streams/jMcAmooE53I-source.mkv` in the lab media folder.
pub fn vp9_1440p_mkv() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("streams/jMcAmooE53I-source.mkv"));
    P.as_str()
}

/// Where generated lab media and cached references live (outside git).
pub fn lab_dir() -> PathBuf {
    let d = std::env::var_os("GMCLIP_LAB_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lab-media"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Video track facts from ffprobe.
#[derive(Debug, Clone)]
pub struct VideoProbe {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub time_base: (u32, u32),
    pub extradata: Vec<u8>,
    pub index: SampleIndex,
}

fn parse_hexdump(text: &str) -> Vec<u8> {
    // ffprobe -show_data lines: "00000000: 0164 001f ffe1 ...  ascii"
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once(": ") else {
            continue;
        };
        let hex: String = rest
            .chars()
            .take(40)
            .filter(|c| c.is_ascii_hexdigit())
            .collect();
        for i in (0..hex.len() / 2 * 2).step_by(2) {
            out.push(u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
        }
    }
    out
}

/// Probes stream `v:0` (or `a:0` with `audio`), building a decode-order index from the
/// packet table (pts, dts, pos, size, flags) and the codec extradata.
pub fn probe(path: &str, stream: &str) -> VideoProbe {
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
    let field = |k: &str| -> String {
        s.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or("")
            .to_string()
    };
    let extradata = match s.find("extradata=") {
        Some(ex_start) => {
            let ex_text = &s[ex_start + "extradata=".len()..];
            let ex_end = ex_text.find("\nextradata_size=").unwrap_or(ex_text.len());
            parse_hexdump(&ex_text[..ex_end])
        }
        None => Vec::new(),
    };
    let tb = field("time_base");
    let (n, d) = tb.split_once('/').unwrap();
    let time_base = (n.parse().unwrap(), d.parse().unwrap());
    let pk = String::from_utf8(run(Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "packet=pts,dts,pos,size,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)))
    .unwrap();
    let mut idx = SampleIndex {
        timescale: time_base.1,
        complete: true,
        ..Default::default()
    };
    for line in pk.lines() {
        let f: Vec<&str> = line.split(',').collect();
        // pts,dts,size,pos,flags
        let pts: i64 = f[0].parse().unwrap();
        let dts: i64 = f[1].parse().unwrap_or(pts);
        idx.dts.push(dts);
        idx.cts_off.push((pts - dts) as i32);
        idx.size.push(f[2].parse().unwrap());
        idx.offset.push(f[3].parse().unwrap());
        idx.sync.push(f[4].starts_with('K'));
    }
    let mut order: Vec<u32> = (0..idx.dts.len() as u32).collect();
    order.sort_by_key(|&i| idx.dts[i as usize] + i64::from(idx.cts_off[i as usize]));
    idx.pts_order = order;
    VideoProbe {
        codec: field("codec_name"),
        width: field("width").parse().unwrap_or(0),
        height: field("height").parse().unwrap_or(0),
        time_base,
        extradata,
        index: idx,
    }
}

/// Reads samples straight out of the file at the probed offsets.
pub struct FileSource {
    pub file: File,
    pub index: SampleIndex,
}

impl FileSource {
    pub fn open(path: &str, index: SampleIndex) -> Self {
        Self {
            file: File::open(path).unwrap(),
            index,
        }
    }
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

/// splitmix64, for seeded PTS selection.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// `n` distinct presentation timestamps chosen with a fixed seed, in the seeded
/// (random) order — the order the seek test visits them.
pub fn seeded_pts(index: &SampleIndex, n: usize, seed: u64) -> Vec<i64> {
    let mut rng = Rng(seed);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let len = index.pts_order.len() as u64;
    while out.len() < n.min(len as usize) {
        let k = index.pts_order[(rng.next() % len) as usize] as usize;
        let pts = index.dts[k] + i64::from(index.cts_off[k]);
        if seen.insert(pts) {
            out.push(pts);
        }
    }
    out
}

// ---- MD5 (RFC 1321), for the plane hashes the goldens use. ----
pub fn md5(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let m: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn md5_vectors() {
    assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"),
        "57edf4a22be3c955ac49da2e2107b67a"
    );
}

/// `(pts, md5(Y), md5(U‖V))` of the frames with the given PTS, decoded by ffmpeg's
/// software decoder (one accurate-seek run per frame), cached under `lab_dir()`.
pub fn golden_md5s(
    path: &str,
    label: &str,
    probe: &VideoProbe,
    pts: &[i64],
) -> Vec<(i64, String, String)> {
    // Keyed by the file's size and mtime and the ffmpeg version too, so a regenerated
    // input or a different reference decoder never reuses stale hashes.
    let meta = std::fs::metadata(path).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ver = String::from_utf8(run(Command::new(FFMPEG).arg("-version"))).unwrap();
    let ver = ver.lines().next().unwrap_or("").replace(' ', "_");
    let key: u64 = md5(format!("{}-{}-{ver}", meta.len(), mtime).as_bytes())[..16]
        .chars()
        .fold(0, |a, c| a * 16 + c.to_digit(16).unwrap() as u64);
    let cache = lab_dir().join(format!("golden-{label}-{key:016x}.txt"));
    let mut sorted: Vec<i64> = pts.to_vec();
    sorted.sort();
    if let Ok(text) = std::fs::read_to_string(&cache) {
        let rows: Vec<(i64, String, String)> = text
            .lines()
            .map(|l| {
                let f: Vec<&str> = l.split(' ').collect();
                (f[0].parse().unwrap(), f[1].to_string(), f[2].to_string())
            })
            .collect();
        if rows.iter().map(|r| r.0).collect::<Vec<_>>() == sorted {
            return rows;
        }
    }
    let (tbn, tbd) = probe.time_base;
    let (w, h) = (probe.width as usize, probe.height as usize);
    let frame_bytes = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    let rows: Vec<(i64, String, String)> = std::thread::scope(|s| {
        let chunks: Vec<&[i64]> = sorted.chunks(sorted.len().div_ceil(4).max(1)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&p| {
                            let secs = p as f64 * tbn as f64 / tbd as f64;
                            let ss = (secs - 0.5).max(0.0);
                            let raw = run(Command::new(FFMPEG).args([
                                "-v",
                                "error",
                                "-threads",
                                "2",
                                "-copyts",
                                "-ss",
                                &format!("{ss:.6}"),
                                "-i",
                                path,
                                "-map",
                                "0:v:0",
                                "-vf",
                                &format!("select=eq(pts\\,{p})"),
                                "-frames:v",
                                "1",
                                "-fps_mode",
                                "passthrough",
                                "-f",
                                "rawvideo",
                                "-pix_fmt",
                                "yuv420p",
                                "-",
                            ]));
                            assert_eq!(raw.len(), frame_bytes, "ffmpeg reference for pts {p}");
                            (p, md5(&raw[..w * h]), md5(&raw[w * h..]))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let text: String = rows
        .iter()
        .map(|(p, y, uv)| format!("{p} {y} {uv}\n"))
        .collect();
    std::fs::write(&cache, text).unwrap();
    rows
}

/// Every frame of a (short) file in presentation order: `(md5(Y), md5(U‖V))`.
pub fn golden_all_md5s(path: &str, probe: &VideoProbe) -> Vec<(String, String)> {
    let (w, h) = (probe.width as usize, probe.height as usize);
    let frame_bytes = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    let raw = run(Command::new(FFMPEG).args([
        "-v",
        "error",
        "-i",
        path,
        "-map",
        "0:v:0",
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-",
    ]));
    assert_eq!(raw.len() % frame_bytes, 0);
    raw.chunks_exact(frame_bytes)
        .map(|f| (md5(&f[..w * h]), md5(&f[w * h..])))
        .collect()
}

/// A synthetic H.264 High clip with B-frames (3, pyramid), GOP 60, closed GOPs, no
/// audio, made once with libx264 (dev tool) under `lab_dir()`.
pub fn synthetic_h264(name: &str, w: u32, h: u32, secs: u32, open_gop: bool) -> String {
    let p = lab_dir().join(name);
    if !p.exists() {
        let tmp = p.with_extension("tmp.mp4");
        let x264 = format!(
            "keyint=60:min-keyint=60:scenecut=0:bframes=3:b-pyramid=normal:open-gop={}",
            u8::from(open_gop)
        );
        run(Command::new(FFMPEG)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={w}x{h}:rate=30,noise=alls=12:allf=t"),
                "-t",
                &secs.to_string(),
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                "20",
                "-x264-params",
                &x264,
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(&tmp));
        std::fs::rename(&tmp, &p).unwrap();
    }
    p.to_string_lossy().into_owned()
}

/// (a′): a 3-minute stream-copy trim of the 1440p MMO restream (H.264 High, B-frames),
/// starting at a keyframe, with no edit list. A trim already cut is used even when the
/// restream itself is no longer on this machine.
pub fn a_prime() -> Option<String> {
    let p = lab_dir().join("a-prime-mmo-1440p-180s.mp4");
    if !p.exists() && !Path::new(mmo_1440p()).exists() {
        return None;
    }
    if !p.exists() {
        let tmp = p.with_extension("tmp.mp4");
        run(Command::new(FFMPEG)
            .args([
                "-v",
                "error",
                "-y",
                "-ss",
                "1200",
                "-i",
                mmo_1440p(),
                "-t",
                "180",
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-c",
                "copy",
                "-avoid_negative_ts",
                "make_zero",
                "-use_editlist",
                "0",
                "-movflags",
                "+faststart",
            ])
            .arg(&tmp));
        std::fs::rename(&tmp, &p).unwrap();
    }
    Some(p.to_string_lossy().into_owned())
}

/// Writes AVCC samples as an Annex-B elementary stream (SPS/PPS before every sync
/// sample) so ffprobe can read what the encoder made (dev-only stand-in for B-04).
pub struct AnnexBSink {
    pub out: Vec<u8>,
    pub param_sets: Vec<Vec<u8>>,
    pub avcc: Vec<u8>,
    pub samples: Vec<(i64, i64, bool, usize)>, // pts, dts, sync, bytes
    pub nal_types: Vec<Vec<u8>>,
    pub durations: Vec<u32>,
}

impl AnnexBSink {
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            param_sets: Vec::new(),
            avcc: Vec::new(),
            samples: Vec::new(),
            nal_types: Vec::new(),
            durations: Vec::new(),
        }
    }
}

impl gmclip_media::backend::VideoSampleSink for AnnexBSink {
    fn video_config(&mut self, avcc: &[u8]) -> Result<(), MediaError> {
        self.avcc = avcc.to_vec();
        assert!(avcc.len() > 6 && avcc[4] & 3 == 3, "4-byte NAL lengths");
        // avcC: 5 header bytes, SPS count & 0x1f, (u16 len, sps)*, PPS count, (u16 len, pps)*.
        let mut i = 5;
        let n_sps = avcc[i] & 0x1f;
        i += 1;
        for _ in 0..n_sps {
            let l = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
            self.param_sets.push(avcc[i + 2..i + 2 + l].to_vec());
            i += 2 + l;
        }
        let n_pps = avcc[i];
        i += 1;
        for _ in 0..n_pps {
            let l = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
            self.param_sets.push(avcc[i + 2..i + 2 + l].to_vec());
            i += 2 + l;
        }
        Ok(())
    }

    fn video_sample(
        &mut self,
        s: &gmclip_media::backend::EncodedVideoSample<'_>,
    ) -> Result<(), MediaError> {
        if s.sync {
            for ps in &self.param_sets {
                self.out.extend_from_slice(&[0, 0, 0, 1]);
                self.out.extend_from_slice(ps);
            }
        }
        let mut i = 0;
        let mut types = Vec::new();
        while i + 4 <= s.data.len() {
            let l = u32::from_be_bytes([s.data[i], s.data[i + 1], s.data[i + 2], s.data[i + 3]])
                as usize;
            let nal = s
                .data
                .get(i + 4..i + 4 + l)
                .expect("NAL length inside the sample");
            types.push(nal.first().map_or(0, |b| b & 0x1f));
            self.out.extend_from_slice(&[0, 0, 0, 1]);
            self.out.extend_from_slice(nal);
            i += 4 + l;
        }
        assert_eq!(i, s.data.len(), "sample is whole NAL units");
        self.nal_types.push(types);
        self.durations.push(s.duration);
        self.samples.push((s.pts, s.dts, s.sync, s.data.len()));
        Ok(())
    }
}

/// Matroska: ffprobe's packet `pos` is the (Simple)Block header (track vint, 16-bit
/// timecode, flags); the frame data starts 4 bytes later for a 1-byte track number
/// without lacing. Checked for every sample, then the offsets are moved to the data.
pub fn mkv_offsets_to_data(path: &str, index: &mut SampleIndex) {
    let mut f = File::open(path).unwrap();
    let mut hdr = [0u8; 4];
    for (i, off) in index.offset.iter_mut().enumerate() {
        f.seek(SeekFrom::Start(*off)).unwrap();
        f.read_exact(&mut hdr).unwrap();
        assert_eq!(hdr[0] & 0x80, 0x80, "sample {i}: 1-byte track vint");
        assert_eq!(hdr[3] & 0x06, 0, "sample {i}: laced block");
        *off += 4;
    }
}

/// (b′) stand-in until the W0-05 corpus lands: a 20 s 720p VP9 clip made with
/// libvpx-vp9 two-pass with alt-ref frames (superframes with hidden alt-refs; libvpx
/// emits no show_existing_frame here — the H-03 test inserts some), in Matroska with
/// cues.
pub fn synthetic_vp9() -> String {
    let p = lab_dir().join("synthetic-vp9-720p-altref.mkv");
    if !p.exists() {
        let tmp = p.with_extension("tmp.mkv");
        let log = lab_dir().join("vp9pass");
        for pass in ["1", "2"] {
            run(Command::new(FFMPEG)
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=1280x720:rate=30,noise=alls=10:allf=t",
                    "-t",
                    "20",
                    "-c:v",
                    "libvpx-vp9",
                    "-b:v",
                    "2M",
                    "-auto-alt-ref",
                    "1",
                    "-lag-in-frames",
                    "25",
                    "-g",
                    "90",
                    "-speed",
                    "4",
                    "-row-mt",
                    "1",
                    "-pass",
                    pass,
                    "-passlogfile",
                ])
                .arg(&log)
                .args([
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-color_range",
                    "tv",
                ])
                .args(if pass == "1" {
                    vec!["-f", "null", "/dev/null"]
                } else {
                    vec![tmp.to_str().unwrap()]
                }));
        }
        std::fs::rename(&tmp, &p).unwrap();
    }
    p.to_string_lossy().into_owned()
}
