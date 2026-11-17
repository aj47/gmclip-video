# S2a — frame-stream preview (SPEC D5, §6.3, §19.3; K1)

**Question.** Is the frame stream (render thread → NV12 at displayed size → loopback
WebSocket → WebGL2 NV12 shader, input records upstream on the same socket) fast enough
that no native surface (S2b) is needed? S2a's exit: BENCH-60 frame stream 1080×1920@30
with 0 drops; input-to-photon recorded.

**Status: 0 drops headless after the join preroll (2026-10-15); exit pending the WKWebView
run** in the real window (screen unlocked; Needs AJ). Before the preroll, frames dropped
at every discontinuous join (first numbers below).

## How it is measured

- Path under test: `app/src-tauri/src/viewer.rs` (render thread) →
  `gmclip_render::export::preview::PreviewSession` (the export's per-frame compositor
  at the displayed size, gizmos drawn in the same pass, NV12 read back) →
  `app/src-tauri/src/frame.rs` (one-connection socket, R-FRAME-1, one-frame mailbox) →
  `app/ui/src/viewer/{framesocket,nv12gl}.ts`.
- Input-to-present: `app/ui/src/viewer/s2a.ts` sends 3 × 60 pointer-move records,
  8–25 ms apart (jittered so samples cover every refresh phase), dragging the largest
  layer's gizmo. Each frame header echoes the newest *pointer* record time it reflects.
  *to draw* = record → WebGL draw of the first frame showing it; *to next rAF* = record →
  the next rAF after that draw. That is a **lower bound on photon time**: compositing, GPU
  and scan-out come after it (S10's flash/camera capture measures them).
- fps: 8 s playback from frame 0 at the displayed size, then 8 s at full 1080×1920 in the
  viewer's *stress* mode (full size while playing; the product shows full size only when
  paused, §6.3). *missing* = output frames in the played span never drawn (render-thread
  skips plus socket coalescing); *dropped* = the render thread's own count.
- Render thread alone: `app/src-tauri/tests/slice.rs` lab tests (`--ignored`), which also
  attribute every frame over 33.3 ms to a join (a part's first frame) or not.

Commands:

```bash
cd app/src-tauri && cargo build --features custom-protocol,dev-bridge
cd ../ui
GMCLIP_S2A=1 GMCLIP_S2A_OUT=/tmp/s2a.json [GMCLIP_S2A_PLAN=<plan>] pnpm exec playwright test s2a   # Chromium via the dev bridge
cd ../src-tauri && GMCLIP_PREVIEW_PLAN=<plan.json> cargo test --features custom-protocol --test slice preview_plan_from_env -- --ignored --nocapture
# The real window (WKWebView), screen unlocked:
HOME=<home with a project> target/debug/gmclip-app --open <home>/Movies/gmclip/Library/<P>.gmclip --s2a
#   → report on stderr (GMCLIP_S2A …) and in <app data>/diagnostics/s2a-*.json, then quits
```

BENCH-60-M0's checked-in plan names `test-media/lab/…`; the run rewrote those three paths
to the lab files on this Mac (`test-media/manifest.toml` rows `a`, `b`, and the 4fadb756
proxy).

## Results (2026-10-14)

Machine: MacBook Pro 18,3 (Apple M1 Pro, 16 GB), macOS 26.2 — **not mac-ref** (the Air).
Build: dev profile with dependencies at opt-level 3 (`app/src-tauri/Cargo.toml`). The Mac
is shared with other build lanes; load average 7–18 during these runs.

### Chromium 153 headless via the dev bridge (WebGL on **SwiftShader**, i.e. software)

| | h79 v4 (1807 frames, 14 parts, VP9 1440p MKV + AAC proxy) | BENCH-60-M0 (1800 frames, 41 parts, 40 discontinuous joins, 4 layers) |
|---|---|---|
| Input → draw (180 samples) | p50 15.1, p95 22.7 ms | p50 23.3, p95 43.7 ms |
| Input → next rAF (photon lower bound) | p50 **30.3**, p95 38.2, max 52.3 ms | p50 38.0, p95 58.3, max 75.8 ms |
| Playback, displayed size (258×458) | 27.0 fps; 216 of 240 drawn; render p50 3.8 / p95 18.6 ms | 23.8 fps; 191 of 239 drawn; render p50 4.7 / p95 18.0 ms |
| Playback, 1080×1920 (stress) | 25.9 fps; 208 of 240 drawn; render p50 6.6 / p95 40.3 ms | 24.8 fps; 199 of 240 drawn; render p50 5.6 / p95 35.4 ms |

### Render thread alone (no socket, no webview), 1080×1920, every frame in order

| Plan | Composite + NV12 readback | Frames over 33.3 ms |
|---|---|---|
| BENCH-60-M0 | p50 **4.0 ms**, p95 16.5, max 254 ms | **35, all of them a part's first frame** (a join) |
| h79 v4 | p50 4.4 ms, p95 19.7, max 387 ms | 33: 12 at joins, 21 inside parts (frames 67–304 and 1091–1586) |
| h79 v4, 90 frames at 540×960 / 1080×1920 | p50 2.6 / 3.4 ms | — |
| far seek (frame 1500, VP9 1440p, ≤ 5 s GOP) | 321–342 ms | — |

### After the join preroll (2026-10-15)

The preview keeps two spare decode streams per source; while playing, the first frame of
each of the next two discontinuous joins is decoded on a spare on its own thread, 1.5 s
ahead plus a frame per 4 samples of its decode chain (a long GOP starts earlier, ≤ 5 s);
at the join the spare becomes the source's stream, already in the new chain
(`gmclip_render::export::apple::preview`, SPEC §6.3). Measured with frames paced at
30 fps (`GMCLIP_PREVIEW_PACED=1`: playback's idle time between frames is when the
preroll decodes), 1080×1920:

| Plan | Before: frames over 33.3 ms | After |
|---|---|---|
| BENCH-60-M0 (40 joins) | 39 (30 at joins; worst 654 ms) | 1–3; **0 at playback joins** (frame 0 is the open, 83–100 ms; others 1–2 stragglers mid-part, ≤ 40 ms) |
| h79 v4 (13 joins) | 38 (11 at joins, 27 mid-part; worst 408 ms) | 1–2; **0 at playback joins** (frame 0, 200–230 ms) |

The frame stream headless (Chromium, SwiftShader), same plans:

| | BENCH-60-M0 before → after | h79 v4 before → after |
|---|---|---|
| Playback, displayed size | 23.8 fps, 191/239 drawn → **29.9 fps, 240/240, 0 dropped** | 27.0 fps, 216/240 → **29.8 fps, 239/240, 0 dropped** |
| Playback, 1080×1920 (stress) | 24.8 fps, 199/240 → **29.9 fps, 240/240, 0 dropped** | 25.9 fps, 208/240 → **30 fps, 240/240, 0 dropped** |
| Input → next rAF p50 / p95 | 38.0 / 58.3 → 31.3 / 38.8 ms | 30.3 / 38.2 → 30.8 / 37.1 ms |

A flat 3 s lookahead was worse (36 slow joins on BENCH): more decode sessions in flight
at once on the shared hardware decoder. Render p50 rises from ~4 to ~6.5 ms with the
preroll threads busy, well inside the budget.

## Reading (first numbers, before the preroll)

- **Steady state fits the budget**: 3–6 ms per 1080×1920 frame on the render thread and
  about 6 ms from socket to draw. Gizmo misregistration is 0 by construction (handles are
  in the streamed frame).
- **S2a's exit is not met: frames drop.** On BENCH-60-M0 every slow frame on the render
  thread is a join, where a new decode chain starts (a seek to a keyframe up to a GOP
  back); §6.3 already specifies the fix (preroll a second decode session ≥ 2 joins
  ahead). h79 also has 21 slow frames inside parts, not explained yet (candidates: VP9
  alt-ref bursts, hold layers' second reader); they need per-frame decode timing before a
  conclusion.
- The browser numbers include SwiftShader's software WebGL, and drops there exceed the
  render thread's own: the upload and draw of 1080×1920 planes in software take a share
  of each frame. The WKWebView window (Metal) is the run that decides K1.
- **K1 is not decided by this evidence.** The native surface (S2b) is only worth trying if
  the WKWebView run, after the join preroll, still misses 0 drops at 30 fps or the
  input-to-photon budget.
- Not measured yet: the WKWebView window (the screen was locked during every run, and
  WebKit stops rAF then), mac-ref, audio-clocked playback (no audio in the preview yet),
  BENCH-60 proper (adds a chroma key, M2), photon time by camera (S10).
