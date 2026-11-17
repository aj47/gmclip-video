# S3-mac — first gmclip exports on Apple silicon (H-12)

**Verdict for the agent-measurable half: GO.** gmclip's own exports (VideoToolbox H.264,
AudioToolbox AAC, gmclip's `Mp4Writer`) have no edit lists, last exactly the plan,
and put every part's audio at the planned sample (0 samples off on all 55 parts
measured). Still open, and AJ's: the M4 Pro and Air timings (H-15), the TikTok /
Shorts / YouTube overlay zones (`zones.json`), and the platform uploads (H-14a/b).

## Method

- Renders: `gmclip render PLAN -o OUT` (release build, the CLI path: B-01/B-02
  demuxers → VT decode → wgpu/Metal compositor + text (H-06) → NV12 → VT encode;
  mix → static loudness → AAC; `Mp4Writer`; B-15 verify before install). Plans: h79
  converted from RT's `render-spec-v4.json` by `gmclip import-rt` (B-13), and
  `schemas/examples/bench-60-m0.renderplan.json` with its sources pointed at AJ's files.
- Facts: `tools/s3_export_facts.py OUT.mp4 PLAN.json` — box walk (edts per track,
  mvhd/mdhd durations), ffprobe, keyframe intervals, 1-s peak over 30 packets in decode
  order, and per part a cross-correlation of the output audio against the plan's audio
  source at the planned sample (±20 ms, to the sample; 0.1 s trimmed at each edge so
  fades and envelopes do not dominate).
- Machine: Apple M1 Pro (10 cores, 16 GB), macOS 26.2 — AJ's MacBook Pro, **not**
  mac-ref. Other build lanes were compiling on it throughout (load average 18–60), so
  the times below are upper bounds, not the R-PERF figures.
- Renders and plans are outside git: `~/Development/gmclip-artifacts/m0-renders/`.

## Results

### Container (SPEC S3-mac: "without `edts`, duration = plan")

| | h79 (golden cut, 14 parts) | BENCH-60-M0 (41 parts, H.264 + VP9 sources) |
|---|---|---|
| plan | 1807 frames = 60.2333 s; 2 891 200 samples | 1800 frames = 60.000 s; 2 880 000 samples |
| top-level boxes | ftyp, moov, free, mdat | ftyp, moov, free, mdat |
| video trak | tkhd + mdia, **no edts**; mdhd 925 184 / 15 360 = 60.2333 s | **no edts**; 921 600 / 15 360 = 60.000 s |
| audio trak | **no edts**; mdhd 2 891 200 / 48 000 = 60.2333 s | **no edts**; 2 880 000 / 48 000 = 60.000 s |
| mvhd | 60 234 / 1000 (ms, rounded up) | 60 000 / 1000 |
| video | H.264 High @ 4.0, `has_b_frames` 2, 1807 packets, start PTS 0 | High @ 4.0, B 2, 1800 packets, start PTS 0 |
| keyframes | every 30 (decode order), no other interval | every 30 |
| 1-s peak / mean | 7.09 / 3.40 Mbps (cap 25; preset 16/24) | 9.13 / 3.63 Mbps |
| audio | AAC-LC 48 kHz, 192.0 kbps, 2824 packets, start PTS 0 | 192.0 kbps, 2813 packets |
| loudness (ebur128 on the file) | −16.0 LUFS, true peak −1.4 dBFS (target −16 / −1.5; verify allows +0.1) | — |
| B-15 verify | 16/16 checks; 10 discontinuous joins, worst 0.00 ms, drift 0 | 16/16 |

The audio ceiling: AAC raises the true peak of the limited mix (+0.1 dB on h25, the one
golden cut that failed verify before the fix); the export now measures the decoded
AAC and lowers the limiter ceiling when it is over (H-10, 5bd80d0).

### A/V alignment (SPEC S3-mac: "A/V xcorr ≤ 1 ms")

| | parts measured | lag, max \|·\| | correlation |
|---|---|---|---|
| h79 | 14 | **0 samples (0.00 ms)** | 0.856–1.000 (0.856 is the part under the +10 dB banana-voice envelope) |
| BENCH-60-M0 | 41 | **0 samples (0.00 ms)** | 0.936–1.000 |

Picture placement is checked separately: decoded output frames against the sources
at the planned frames, content SSIM ≥ 0.998 (H-10 lab), and two renders of one plan
give identical encoder-input hashes (decision 0005).

### Against RT's shipped h79-v4

| | gmclip h79 | RT h79-v4.mp4 |
|---|---|---|
| duration | 60.2333 s | 60.2333 s |
| whole-frame SSIM (ffmpeg `ssim`, ours vs RT) | mean 0.9963, min 0.9948 over 1807 frames | — |
| PSNR | Y 43.1 dB, avg 43.6 dB (min 42.6) | — |
| total bitrate | 3.59 Mbps | 1.64 Mbps (x264) |

Text alone against RT's libass is in STATUS (H-06: SSIM ≥ 0.9946 on 175 frames).

### Export time (Apple M1 Pro, loaded — see Method)

| | runs | wall time |
|---|---|---|
| BENCH-60-M0 via `gmclip render` (text on) | 3 | 20.3, 20.7, 21.0 s |
| h79 via `gmclip render` (text on) | 3 | 19.6, 20.0, 21.6 s |
| BENCH-60-M0, lab, text off, earlier quieter runs | — | 15.6–19 s |
| the 11 golden cuts, one pass each | 1 | 242 s total (4.7–46 s), load 24–60 |

SPEC budgets (mac-ref, R-PERF-1): BENCH-60-M0 ≤ 30 s, h79 ≤ 15 s. h79 on this loaded M1
Pro misses 15 s; it needs the quiet mac-ref / Air run (H-15) before it is a finding.
A font that cannot be opened is now refused before any work (0.04 s, step `text`).

### Overlay zones

`zones.json` carries Instagram's rects from SPEC (not re-measured). TikTok, Shorts and
YouTube need AJ's screenshots — see STATUS "Needs AJ". VMAF: not run (optional at M0).

## Re-run

```bash
cargo build -p gmclip-cli --release
target/release/gmclip import-rt /…/h79-v3-2026-10-04/render-spec-v4.json \
    --sources test-media/rt-sources.toml -o /tmp/h79.renderplan.json
target/release/gmclip render /tmp/h79.renderplan.json -o /tmp/h79.mp4
python3 tools/s3_export_facts.py /tmp/h79.mp4 /tmp/h79.renderplan.json
```
