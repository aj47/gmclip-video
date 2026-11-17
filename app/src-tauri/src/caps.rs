//! The capability probe line per source (R-PLAT-1 Mac; backlog H.09/U.15): what this Mac
//! can do with a file's video and each audio track, and for anything it cannot, the exact
//! fix. gmclip never installs a codec; the fixes are settings in the recorder (OBS) or a
//! converted copy.
//!
//! What this build reads: H.264 and VP9 video (VP9 through Apple's VP9 decoder, Apple
//! silicon and T2 Macs on macOS 11+); AAC audio for transcripts; exports mix AAC or PCM,
//! mono or stereo, at any rate (converted to 48 kHz, decision 0051).

use serde::Serialize;

/// The machine in the app's words: "this Mac" on macOS, "this PC" on Windows (the UI's
/// `platform.thisMachine`).
pub const THIS_MACHINE: &str = if cfg!(windows) { "this PC" } else { "this Mac" };

/// One line of the sheet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    /// `ok`, `warn` (opens, something is limited) or `no` (cannot be read).
    pub level: &'static str,
    pub text: String,
    /// The exact fix, when there is one.
    pub fix: Option<String>,
}

fn line(level: &'static str, text: impl Into<String>, fix: Option<&str>) -> Line {
    Line {
        level,
        text: text.into(),
        fix: fix.map(str::to_owned),
    }
}

#[cfg(not(windows))]
const OBS_H264: &str = "For your next recordings: OBS › Settings › Output › Recording › Video Encoder: “Apple VT H264 Hardware Encoder”. For this file: convert a copy to H.264 with HandBrake (handbrake.fr, preset “Fast 1080p30”) and open the copy. gmclip never installs codecs.";
/// OBS on Windows names the GPU's encoder (NVIDIA, AMD or Intel).
#[cfg(windows)]
const OBS_H264: &str = "For your next recordings: OBS › Settings › Output › Recording › Video Encoder: your GPU's H.264 encoder (“NVIDIA NVENC H.264”, “AMD HW H.264” or “QuickSync H.264”). For this file: convert a copy to H.264 with HandBrake (handbrake.fr, preset “Fast 1080p30”) and open the copy. gmclip never installs codecs.";

/// The video line. `vp9_decoder`: whether this Mac has Apple's VP9 decoder.
pub fn video(codec: &str, w: u32, h: u32, vp9_decoder: bool) -> Line {
    match codec {
        "h264" => line("ok", format!("H.264 {w}×{h}: plays and exports"), None),
        "vp9" if vp9_decoder => line(
            "ok",
            format!("VP9 {w}×{h}: plays and exports ({VP9_DECODER})"),
            None,
        ),
        "vp9" => line("no", format!("VP9 {w}×{h}: {NO_VP9}"), Some(OBS_H264)),
        "hevc" => line(
            "no",
            format!("HEVC {w}×{h}: gmclip does not decode HEVC yet"),
            Some(OBS_H264),
        ),
        "av1" => line(
            "no",
            format!("AV1 {w}×{h}: gmclip does not decode AV1 yet"),
            Some(OBS_H264),
        ),
        other => line(
            "no",
            format!("{other} video: gmclip cannot read it"),
            Some(
                "Convert a copy to H.264 MP4 with HandBrake (handbrake.fr, preset “Fast 1080p30”) and open the copy. gmclip never installs codecs.",
            ),
        ),
    }
}

#[cfg(not(windows))]
const VP9_DECODER: &str = "Apple's VP9 decoder";
#[cfg(windows)]
const VP9_DECODER: &str = "on the GPU";
#[cfg(not(windows))]
const NO_VP9: &str = "this Mac has no VP9 decoder (Apple's comes with Apple silicon and T2 Macs on macOS 11 or later)";
/// Microsoft's VP9 decoder runs on the GPU only (Windows' VP9 Video Extensions).
#[cfg(windows)]
const NO_VP9: &str = "this PC has no VP9 decoder (Windows decodes VP9 on the GPU, with the VP9 Video Extensions from the Microsoft Store)";

#[cfg(not(windows))]
const OBS_AAC: &str = "For your next recordings: OBS › Settings › Output › Recording › Audio Encoder: “CoreAudio AAC”. For this file: convert a copy with HandBrake (handbrake.fr; Audio: AAC) and open the copy.";
#[cfg(windows)]
const OBS_AAC: &str = "For your next recordings: OBS › Settings › Output › Recording › Audio Encoder: “FFmpeg AAC”. For this file: convert a copy with HandBrake (handbrake.fr; Audio: AAC) and open the copy.";
const OBS_48K: &str = "For your next recordings: OBS › Settings › Audio › General › Sample Rate: 48 kHz and Channels: Stereo.";

/// One audio track's line.
pub fn audio(codec: &str, rate: u32, channels: u16) -> Line {
    let khz = if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", f64::from(rate) / 1000.0)
    };
    let layout = match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        n => format!("{n} channels"),
    };
    match codec {
        "aac" if rate == 48_000 && (channels == 1 || channels == 2) => line(
            "ok",
            format!("AAC {khz} {layout}: transcribed and exported"),
            None,
        ),
        // decision 0051: another rate is converted to 48 kHz as it is read
        "aac" if channels == 1 || channels == 2 => line(
            "ok",
            format!("AAC {khz} {layout}: transcribed and exported (converted to 48 kHz)"),
            None,
        ),
        "aac" => line(
            "warn",
            format!(
                "AAC {khz} {layout}: transcribed, but exports mix mono or stereo, so this track cannot be exported yet"
            ),
            Some(OBS_48K),
        ),
        "opus" => line(
            "warn",
            format!("Opus {khz} {layout}: not read yet (transcripts read AAC)"),
            Some(OBS_AAC),
        ),
        "pcm" if channels == 1 || channels == 2 => line(
            "warn",
            format!("PCM {khz} {layout}: exported, but not transcribed (transcripts read AAC)"),
            Some(OBS_AAC),
        ),
        "pcm" => line(
            "warn",
            format!("PCM {khz} {layout}: not read yet (exports mix mono or stereo)"),
            Some(OBS_AAC),
        ),
        other => line(
            "warn",
            format!("{other} audio: not read yet (transcripts read AAC)"),
            Some(OBS_AAC),
        ),
    }
}

/// The note for a file with one audio track (SPEC §5.1).
pub const ONE_TRACK: &str = "One audio track: game audio and music will be in the transcript too. To keep your voice apart, record your mic on its own track: OBS › Settings › Output › Recording › Audio Track: tick 1 and 2, then Edit › Advanced Audio Properties: your mic on track 2 only.";

/// No audio at all.
pub const NO_AUDIO: &str = "No audio track: there will be no transcript.";

/// Whether this Mac has Apple's VP9 decoder (on Windows: whether a VP9 session opens on
/// the GPU, as the app's own decode does; asked once).
pub fn vp9_decoder() -> bool {
    #[cfg(target_os = "macos")]
    {
        gmclip_gpu::apple::vt_decode::hardware_decode_supported(true)
    }
    #[cfg(windows)]
    {
        static VP9: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *VP9.get_or_init(|| gpu_session(gmclip_gpu::windows::VideoCodec::Vp9))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

/// Whether a decode session for `codec` opens on the GPU (Windows).
#[cfg(windows)]
fn gpu_session(codec: gmclip_gpu::windows::VideoCodec) -> bool {
    gmclip_gpu::windows::H264Decoder::open(codec, 1920, 1080, true).is_ok()
}

// ---------------------------------------------------------------------------------------
// The startup probe (R-PLAT-1 "at startup"; H.09): what VideoToolbox offers on this Mac,
// asked once at launch, and the lines Home shows for it.
// ---------------------------------------------------------------------------------------

/// What VideoToolbox offers in hardware on this Mac.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Probe {
    pub h264_decode: bool,
    pub h264_encode: bool,
    /// Apple's supplemental VP9 decoder (Apple silicon and T2 Macs, macOS 11+).
    pub vp9_decode: bool,
    /// Reported only: gmclip does not read HEVC at M2.
    pub hevc_decode: bool,
    pub hevc_encode: bool,
    /// Reported only: gmclip does not read AV1 at M2 (M3 and later decode it).
    pub av1_decode: bool,
}

/// The Mac the probe ran on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Machine {
    /// `machdep.cpu.brand_string`, e.g. "Apple M1 Pro" or "Intel(R) Core(TM) i7-8569U …".
    pub chip: String,
    pub apple_silicon: bool,
    /// `kern.osproductversion`, e.g. "14.6".
    pub macos: String,
    /// Windows: its version (`ver`), e.g. "10.0.26200.6584"; the chip is the GPU there.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub windows: String,
}

/// The startup lines for a machine and its probe: the Mac, then H.264, VP9, HEVC and AV1,
/// each with the exact fix where gmclip cannot do something. Pure: the Intel / M1 / M3
/// matrix is unit-tested on any machine.
pub fn startup_lines(m: &Machine, p: &Probe) -> Vec<Line> {
    let mut out = Vec::new();
    let chip = if m.chip.is_empty() {
        "this Mac".to_owned()
    } else {
        m.chip.clone()
    };
    let mac = if m.macos.is_empty() {
        chip.clone()
    } else {
        format!("{chip}, macOS {}", m.macos)
    };
    out.push(line("ok", mac, None));
    out.push(match (p.h264_decode, p.h264_encode) {
        (true, true) => line("ok", "H.264: plays and exports in hardware", None),
        (true, false) => line(
            "warn",
            "H.264: plays in hardware; no hardware encoder answered, so exports use the slower software encoder",
            Some("Quit other apps that are recording or encoding video (OBS, screen recorders, video calls), then export again; the hardware encoder is shared."),
        ),
        (false, true) => line(
            "warn",
            "H.264: exports in hardware; plays with the software decoder, so 1440p and 60 fps sources may drop frames while you edit",
            Some("For your next recordings: OBS › Settings › Output › Recording › Recording Format: 1080p at 30 fps (Video › Output Resolution and FPS)."),
        ),
        (false, false) => line(
            "warn",
            "H.264: no hardware decoder or encoder answered: playback and exports use software and are slower",
            Some("Quit other apps that are recording or encoding video (OBS, screen recorders, video calls) and restart gmclip."),
        ),
    });
    out.push(if p.vp9_decode {
        line("ok", "VP9: plays and exports (Apple's VP9 decoder)", None)
    } else {
        line(
            "no",
            if m.apple_silicon {
                "VP9: Apple's VP9 decoder did not answer on this Mac, so VP9 recordings cannot be opened".to_owned()
            } else {
                "VP9: this Mac has no VP9 decoder (Apple's comes with Apple silicon and T2 Macs on macOS 11 or later), so VP9 recordings cannot be opened".to_owned()
            },
            Some(OBS_H264),
        )
    });
    out.push(line(
        "warn",
        if p.hevc_decode {
            "HEVC: this Mac decodes it in hardware, but gmclip does not read HEVC yet"
        } else {
            "HEVC: no hardware decoder on this Mac, and gmclip does not read HEVC yet"
        },
        Some(OBS_H264),
    ));
    out.push(line(
        "warn",
        if p.av1_decode {
            "AV1: this Mac decodes it in hardware, but gmclip does not read AV1 yet"
        } else {
            "AV1: no hardware decoder on this Mac (Apple's comes with M3 and later), and gmclip does not read AV1 yet"
        },
        Some(OBS_H264),
    ));
    out
}

/// The startup lines on Windows: the PC (its GPU and Windows version), then H.264 and VP9
/// as gmclip runs them (decoded on the GPU through Media Foundation, else in software;
/// exported with Microsoft's encoder, which meets the presets), HEVC and AV1 reported.
/// Pure, like [`startup_lines`].
pub fn startup_lines_windows(m: &Machine, p: &Probe) -> Vec<Line> {
    let mut out = Vec::new();
    let gpu = if m.chip.is_empty() {
        "this PC".to_owned()
    } else {
        m.chip.clone()
    };
    out.push(line(
        "ok",
        if m.windows.is_empty() {
            gpu
        } else {
            format!("{gpu}, Windows {}", m.windows)
        },
        None,
    ));
    out.push(if p.h264_decode {
        line(
            "ok",
            "H.264: plays on the GPU; exports with Microsoft's encoder",
            None,
        )
    } else {
        line(
            "warn",
            "H.264: no GPU decoder answered, so playback uses the software decoder and 1440p and 60 fps sources may drop frames while you edit",
            Some("Update the graphics driver (NVIDIA, AMD or Intel), then restart gmclip."),
        )
    });
    out.push(if p.vp9_decode {
        line("ok", "VP9: plays and exports (on the GPU)", None)
    } else {
        line(
            "no",
            format!("VP9: {NO_VP9}, so VP9 recordings cannot be opened"),
            Some(OBS_H264),
        )
    });
    out.push(line(
        "warn",
        if p.hevc_decode {
            "HEVC: this PC decodes it on the GPU, but gmclip does not read HEVC yet"
        } else {
            "HEVC: no GPU decoder on this PC, and gmclip does not read HEVC yet"
        },
        Some(OBS_H264),
    ));
    out.push(line(
        "warn",
        if p.av1_decode {
            "AV1: this PC decodes it on the GPU, but gmclip does not read AV1 yet"
        } else {
            "AV1: no GPU decoder on this PC, and gmclip does not read AV1 yet"
        },
        Some(OBS_H264),
    ));
    out
}

/// Asks VideoToolbox (macOS) or Media Foundation (Windows; elsewhere nothing is offered
/// until M6).
pub fn probe_now() -> Probe {
    #[cfg(target_os = "macos")]
    {
        use gmclip_gpu::apple::vt_decode::{ProbeCodec, hardware_decode};
        use gmclip_gpu::apple::vt_encode::hardware_encode_supported;
        Probe {
            h264_decode: hardware_decode(ProbeCodec::H264),
            h264_encode: hardware_encode_supported(false),
            vp9_decode: hardware_decode(ProbeCodec::Vp9),
            hevc_decode: hardware_decode(ProbeCodec::Hevc),
            hevc_encode: hardware_encode_supported(true),
            av1_decode: hardware_decode(ProbeCodec::Av1),
        }
    }
    #[cfg(windows)]
    {
        Probe {
            h264_decode: gpu_session(gmclip_gpu::windows::VideoCodec::H264),
            // exports use Microsoft's software encoder (the GPU's, through Media
            // Foundation, misses the presets' level and B-frames)
            h264_encode: false,
            vp9_decode: vp9_decoder(),
            hevc_decode: gmclip_gpu::windows::hardware_decoder_listed_for("hevc"),
            hevc_encode: false,
            av1_decode: gmclip_gpu::windows::hardware_decoder_listed_for("av1"),
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Probe::default()
    }
}

/// This PC's GPU and Windows version (`ver`, the OS's own command).
#[cfg(windows)]
pub fn machine_now() -> Machine {
    let ver = std::process::Command::new("cmd")
        .args(["/d", "/c", "ver"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    // "Microsoft Windows [Version 10.0.26200.6584]"
    let windows = ver
        .split("Version ")
        .nth(1)
        .and_then(|v| v.split(']').next())
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .take(32)
        .collect();
    Machine {
        chip: gmclip_gpu::windows::D3dDevice::shared()
            .map(|d| {
                d.name()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(120)
                    .collect()
            })
            .unwrap_or_default(),
        apple_silicon: false,
        macos: String::new(),
        windows,
    }
}

/// This Mac's chip and macOS version (`/usr/sbin/sysctl`, the OS's own tool).
#[cfg(not(windows))]
pub fn machine_now() -> Machine {
    let read = |k: &str| {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", k])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(120)
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    Machine {
        chip: read("machdep.cpu.brand_string"),
        // the hardware, not this process (a Rosetta build still runs on Apple silicon)
        apple_silicon: read("hw.optional.arm64") == "1",
        macos: read("kern.osproductversion"),
        windows: String::new(),
    }
}

static STARTUP: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();

/// The startup probe: run once (at launch, on its own thread; or by the first caller) and
/// kept for the session.
pub fn startup() -> serde_json::Value {
    STARTUP
        .get_or_init(|| {
            let m = machine_now();
            let p = probe_now();
            let lines = if cfg!(windows) {
                startup_lines_windows(&m, &p)
            } else {
                startup_lines(&m, &p)
            };
            serde_json::json!({"machine": m, "probe": p, "lines": lines})
        })
        .clone()
}

/// Starts the probe in the background at launch (VideoToolbox answers in milliseconds,
/// but never on the window's thread).
pub fn probe_at_startup() {
    let _ = std::thread::Builder::new()
        .name("gmclip-caps".into())
        .spawn(|| {
            let _ = startup();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intel() -> (Machine, Probe) {
        (
            Machine {
                chip: "Intel(R) Core(TM) i7-8569U CPU @ 2.80GHz".into(),
                apple_silicon: false,
                macos: "13.6".into(),
                windows: String::new(),
            },
            Probe {
                h264_decode: true,
                h264_encode: true,
                vp9_decode: false,
                hevc_decode: true,
                hevc_encode: true,
                av1_decode: false,
            },
        )
    }

    fn m1() -> (Machine, Probe) {
        (
            Machine {
                chip: "Apple M1".into(),
                apple_silicon: true,
                macos: "14.6".into(),
                windows: String::new(),
            },
            Probe {
                h264_decode: true,
                h264_encode: true,
                vp9_decode: true,
                hevc_decode: true,
                hevc_encode: true,
                av1_decode: false,
            },
        )
    }

    fn m3() -> (Machine, Probe) {
        (
            Machine {
                chip: "Apple M3 Pro".into(),
                apple_silicon: true,
                macos: "15.1".into(),
                windows: String::new(),
            },
            Probe {
                av1_decode: true,
                ..m1().1
            },
        )
    }

    fn levels(l: &[Line]) -> Vec<&str> {
        l.iter().map(|x| x.level).collect()
    }

    /// H.09: the message matrix on an Intel Mac, an M1 and an M3.
    #[test]
    fn startup_matrix_intel_m1_m3() {
        let (m, p) = intel();
        let l = startup_lines(&m, &p);
        assert_eq!(levels(&l), ["ok", "ok", "no", "warn", "warn"]);
        assert_eq!(
            l[0].text,
            "Intel(R) Core(TM) i7-8569U CPU @ 2.80GHz, macOS 13.6"
        );
        assert_eq!(l[1].text, "H.264: plays and exports in hardware");
        assert!(l[2].text.starts_with(
            "VP9: this Mac has no VP9 decoder (Apple's comes with Apple silicon and T2 Macs"
        ));
        assert_eq!(l[2].fix.as_deref(), Some(OBS_H264));
        assert_eq!(
            l[3].text,
            "HEVC: this Mac decodes it in hardware, but gmclip does not read HEVC yet"
        );
        assert_eq!(
            l[4].text,
            "AV1: no hardware decoder on this Mac (Apple's comes with M3 and later), and gmclip does not read AV1 yet"
        );

        let (m, p) = m1();
        let l = startup_lines(&m, &p);
        assert_eq!(levels(&l), ["ok", "ok", "ok", "warn", "warn"]);
        assert_eq!(l[0].text, "Apple M1, macOS 14.6");
        assert_eq!(l[2].text, "VP9: plays and exports (Apple's VP9 decoder)");
        assert!(l[2].fix.is_none());
        assert!(l[4].text.starts_with("AV1: no hardware decoder"));

        let (m, p) = m3();
        let l = startup_lines(&m, &p);
        assert_eq!(levels(&l), ["ok", "ok", "ok", "warn", "warn"]);
        assert_eq!(l[0].text, "Apple M3 Pro, macOS 15.1");
        assert_eq!(
            l[4].text,
            "AV1: this Mac decodes it in hardware, but gmclip does not read AV1 yet"
        );
        // every line that is not ok carries the exact fix, and none installs anything
        for (m, p) in [intel(), m1(), m3()] {
            for x in startup_lines(&m, &p).iter().filter(|x| x.level != "ok") {
                let f = x.fix.as_deref().unwrap();
                let offered = f.replace("gmclip never installs codecs", "");
                assert!(!offered.to_lowercase().contains("install"), "{f}");
            }
        }
    }

    #[test]
    fn startup_without_a_hardware_encoder_or_decoder() {
        let (m, p) = intel();
        let l = startup_lines(
            &m,
            &Probe {
                h264_encode: false,
                ..p
            },
        );
        assert_eq!(l[1].level, "warn");
        assert!(l[1].text.contains("slower software encoder"));
        assert!(l[1].fix.as_deref().unwrap().contains("Quit other apps"));
        let l = startup_lines(
            &m,
            &Probe {
                h264_decode: false,
                h264_encode: false,
                ..p
            },
        );
        assert!(
            l[1].text
                .starts_with("H.264: no hardware decoder or encoder")
        );
        // an Apple silicon Mac whose VP9 decoder did not answer says so, not "no T2"
        let (m, p) = m1();
        let l = startup_lines(
            &m,
            &Probe {
                vp9_decode: false,
                ..p
            },
        );
        assert_eq!(l[2].level, "no");
        assert!(l[2].text.contains("did not answer"));
        // an unknown machine still reads as a sentence
        let l = startup_lines(&Machine::default(), &p);
        assert_eq!(l[0].text, "this Mac");
    }

    // the lines it shares with macOS read for the platform they are built on
    #[cfg(windows)]
    #[test]
    fn startup_lines_on_windows() {
        let m = Machine {
            chip: "NVIDIA GeForce RTX 5080".into(),
            windows: "10.0.26200.6584".into(),
            ..Machine::default()
        };
        let p = Probe {
            h264_decode: true,
            vp9_decode: true,
            hevc_decode: true,
            ..Probe::default()
        };
        let l = startup_lines_windows(&m, &p);
        assert_eq!(levels(&l), ["ok", "ok", "ok", "warn", "warn"]);
        assert_eq!(
            l[0].text,
            "NVIDIA GeForce RTX 5080, Windows 10.0.26200.6584"
        );
        assert!(l[1].text.contains("on the GPU"));
        assert!(l[3].text.contains("decodes it on the GPU"));
        assert!(l[4].text.starts_with("AV1: no GPU decoder"));
        let none = startup_lines_windows(&Machine::default(), &Probe::default());
        assert_eq!(levels(&none), ["ok", "warn", "no", "warn", "warn"]);
        assert_eq!(none[0].text, "this PC");
        assert!(none[2].text.contains("no VP9 decoder"));
        for line in l.iter().chain(&none) {
            assert!(!line.text.contains("Mac"), "{}", line.text);
            // never asked to install anything (the fixes say gmclip never installs)
            if let Some(f) = &line.fix {
                let f = f.to_lowercase().replace("never installs", "");
                assert!(!f.contains("install"), "{f}");
            }
        }
    }

    #[test]
    fn video_lines() {
        assert_eq!(video("h264", 1920, 1080, false).level, "ok");
        assert_eq!(video("vp9", 2560, 1440, true).level, "ok");
        let v = video("vp9", 2560, 1440, false);
        assert_eq!(v.level, "no");
        assert!(v.text.contains("no VP9 decoder"));
        assert!(v.fix.as_deref().unwrap().contains(if cfg!(windows) {
            "NVIDIA NVENC H.264"
        } else {
            "Apple VT H264 Hardware Encoder"
        }));
        for c in ["hevc", "av1", "prores"] {
            let l = video(c, 1, 1, true);
            assert_eq!(l.level, "no", "{c}");
            let f = l.fix.unwrap();
            assert!(
                f.contains("HandBrake") && f.contains("never installs codecs"),
                "{c}"
            );
        }
    }

    #[test]
    fn audio_lines() {
        assert_eq!(audio("aac", 48_000, 2).level, "ok");
        // decision 0051: another rate is converted, more than two channels is not mixed
        let a = audio("aac", 44_100, 2);
        assert_eq!(a.level, "ok");
        assert!(a.text.contains("44.1 kHz stereo") && a.text.contains("converted"));
        assert!(a.fix.is_none());
        let a = audio("aac", 48_000, 6);
        assert_eq!(a.level, "warn");
        assert!(a.fix.unwrap().contains("Channels: Stereo"));
        assert!(audio("aac", 48_000, 1).text.contains("mono"));
        assert!(audio("pcm", 44_100, 2).text.contains("exported"));
        for c in ["opus", "pcm", "mp3"] {
            let l = audio(c, 48_000, 2);
            assert_eq!(l.level, "warn");
            let aac = if cfg!(windows) {
                "FFmpeg AAC"
            } else {
                "CoreAudio AAC"
            };
            assert!(l.fix.unwrap().contains(aac), "{c}");
        }
    }
}
