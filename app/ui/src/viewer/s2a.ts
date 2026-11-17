// S2a (SPEC §19.3, K1): frame-stream preview measured in the real webview.
// - Input-to-present: synthetic pointer drags on a layer gizmo; each frame echoes the
//   newest pointer record time it reflects. "draw" is when WebGL drew it; "next_raf" is
//   the next rAF callback after that draw: a lower bound on photon time (compositing,
//   GPU and scan-out come after it; S10's flash capture measures the rest).
// - fps: playback at the displayed size and at full 1080×1920, frames drawn per second
//   and frames dropped (render thread skips + socket coalescing).
import { actions, type CutView } from "../actions";
import { FrameSocket, RecordKind } from "./framesocket";

export interface DrawSample {
  frame: number;
  w: number;
  h: number;
  echoT: number;
  renderUs: number;
  dropped: number;
  receivedAt: number;
  drawAt: number;
  photonAt: number;
}

export const drawLog: DrawSample[] = [];
export function logDraw(s: DrawSample) {
  drawLog.push(s);
  if (drawLog.length > 20_000) drawLog.splice(0, 10_000);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const pct = (xs: number[], p: number) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return Math.round(s[Math.min(s.length - 1, Math.floor(p * s.length))]! * 10) / 10;
};

async function playFor(ms: number) {
  const start = drawLog.length;
  await actions.viewerSeek(0);
  await sleep(300);
  const t0 = performance.now();
  await actions.viewerPlay();
  await sleep(ms);
  await actions.viewerPause();
  const t1 = performance.now();
  const s = drawLog.slice(start).filter((d) => d.drawAt >= t0 && d.drawAt <= t1);
  const frames = s.length;
  const secs = (t1 - t0) / 1000;
  // the header carries a cumulative count (mod 2^16)
  const dropped = s.length > 1 ? (s[s.length - 1]!.dropped - s[0]!.dropped + 65536) % 65536 : 0;
  const span = s.length > 1 ? s[s.length - 1]!.frame - s[0]!.frame + 1 : 0;
  return {
    seconds: Math.round(secs * 100) / 100,
    drawn: frames,
    fps: Math.round((frames / secs) * 10) / 10,
    frames_spanned: span,
    missing: Math.max(0, span - frames),
    dropped_reported: dropped,
    size: s[0] ? [s[0].w, s[0].h] : null,
    render_ms_p50: pct(s.map((d) => d.renderUs / 1000), 0.5),
    render_ms_p95: pct(s.map((d) => d.renderUs / 1000), 0.95),
    // socket message → WebGL draw: mostly waiting for the next rAF, not transfer
    receive_to_draw_ms_p50: pct(s.map((d) => d.drawAt - d.receivedAt), 0.5),
  };
}

export async function runS2a(cut: CutView, log: (s: string) => void) {
  const fs = await FrameSocket.get();
  await actions.viewerFull(false);
  await actions.viewerPause();
  const part = cut.parts[0]!;
  const f = part.out[0] + Math.min(15, part.out[1] - part.out[0] - 1);
  await actions.viewerSeek(f);
  await sleep(400);
  const layer = [...part.layers].filter((l) => l.visible).sort((a, b) => b.place[2] * b.place[3] - a.place[2] * a.place[3])[0];
  const lat: { draw: number[]; photon: number[] } = { draw: [], photon: [] };
  if (layer) {
    const cx = (layer.place[0] + layer.place[2] / 2) / cut.canvas.w;
    const cy = (layer.place[1] + layer.place[3] / 2) / cut.canvas.h;
    log("S2a: input-to-photon (gizmo drag)…");
    for (let round = 0; round < 3; round++) {
      fs.send(RecordKind.Down, { buttons: 1, x: cx, y: cy, t: performance.now() });
      await sleep(80);
      const sent: number[] = [];
      for (let i = 0; i < 60; i++) {
        const t = performance.now();
        const a = (i / 60) * Math.PI * 2;
        fs.send(RecordKind.Move, { buttons: 1, x: cx + Math.cos(a) * 0.03, y: cy + Math.sin(a) * 0.02, t });
        sent.push(t);
        // jittered, so samples cover every phase of the display's refresh
        await sleep(8 + Math.random() * 17);
      }
      await sleep(250);
      fs.send(RecordKind.Cancel, { t: performance.now() });
      fs.send(RecordKind.Up, { t: performance.now() });
      for (const t of sent) {
        const d = drawLog.find((s) => s.echoT >= t - 0.001 && s.drawAt >= t);
        if (d) {
          lat.draw.push(d.drawAt - t);
          if (d.photonAt > 0) lat.photon.push(d.photonAt - t);
        }
      }
      await sleep(200);
    }
  }
  log("S2a: playback at the displayed size…");
  const display = await playFor(8000);
  log("S2a: playback at full 1080×1920 (stress mode: full size while playing)…");
  await actions.viewerFull(true);
  await actions.viewerStress(true);
  await sleep(500);
  const full = await playFor(8000);
  await actions.viewerStress(false);
  await actions.viewerFull(false);
  const probe = document.createElement("canvas").getContext("webgl2");
  const dbg = probe?.getExtension("WEBGL_debug_renderer_info");
  const report = {
    ua: navigator.userAgent,
    webgl_renderer: dbg && probe ? String(probe.getParameter(dbg.UNMASKED_RENDERER_WEBGL)) : null,
    when: new Date().toISOString(),
    dpr: devicePixelRatio,
    canvas: cut.canvas,
    latency_ms: {
      samples: lat.draw.length,
      to_draw_p50: pct(lat.draw, 0.5),
      to_draw_p95: pct(lat.draw, 0.95),
      to_next_raf_p50: pct(lat.photon, 0.5),
      to_next_raf_p95: pct(lat.photon, 0.95),
      to_next_raf_max: pct(lat.photon, 1),
    },
    play_display: display,
    play_full: full,
  };
  log(`S2a done: ${JSON.stringify(report)}`);
  return report;
}
