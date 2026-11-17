// R-TL-1 [M2] (SPEC §5.3, §13): the timeline pans, zooms and drags at 60 fps with 300
// parts and 500 overlays. The cut is h79's RenderPlan re-tiled into 300 parts (each of its
// 14 takes split into consecutive pieces, fades only where the takes join) with 500
// section pills added (greedy-packed into overlay lanes), imported as a project. Over
// ~10 s of scripted pan (wheel), zoom (⌥-wheel) and drags (a part's ghost, a trim edge,
// each cancelled), the page records every animation-frame interval and every timeline
// draw. Gates: no long animation frame (≥ 50 ms, Chrome's LoAF) with work from the page,
// 99 % of frames on the 60 Hz clock, a draw's p95 under 8 ms; the numbers are printed
// for STATUS.md. Input arrives faster than frames here (CDP), so this also checks the
// timeline applies at most one move and one wheel sum per frame. This is
// Chromium against the dev bridge; mac-ref (the Air) and WKWebView timing are AJ.02.
import { expect, test } from "@playwright/test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labPlan } from "./lab";

const H79 = labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json"));
const PARTS = 300;
const OVERLAYS = 500;

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

type Json = Record<string, unknown>;
interface Part {
  id: string;
  out: [number, number];
  picture: { first_frame: number } & Json;
  audio: { first_sample: number; fade_in?: unknown; fade_out?: unknown } & Json;
  native: { frames: [number, number] } & Json;
}

/** h79 re-tiled into `PARTS` parts plus `OVERLAYS` pills: a valid RenderPlan. */
function bigPlan(): string {
  const p = JSON.parse(readFileSync(H79, "utf8")) as { output: { frames: number }; parts: Part[]; text_events: (Json & { id: string; role: string; out: [number, number]; runs: Json[] })[]; meta: Json };
  const total = p.output.frames;
  const spf = 48000 / 30;
  // pieces per take, proportional to its length, adding up to PARTS
  const want = p.parts.map((q) => Math.max(1, Math.round(((q.out[1] - q.out[0]) / total) * PARTS)));
  while (want.reduce((a, b) => a + b, 0) > PARTS) want[want.indexOf(Math.max(...want))]! -= 1;
  while (want.reduce((a, b) => a + b, 0) < PARTS) want[want.indexOf(Math.max(...want))]! += 1;
  const parts: Part[] = [];
  p.parts.forEach((q, i) => {
    const len = q.out[1] - q.out[0];
    const n = want[i]!;
    for (let k = 0; k < n; k++) {
      const o = Math.floor((k * len) / n);
      const e = Math.floor(((k + 1) * len) / n);
      const c = structuredClone(q);
      c.id = `${q.id}-${k}`;
      c.out = [q.out[0] + o, q.out[0] + e];
      c.picture.first_frame = q.picture.first_frame + o;
      c.audio.first_sample = q.audio.first_sample + o * spf;
      c.native.frames = [q.native.frames[0] + o, q.native.frames[0] + e];
      // the joins inside a take are continuous: no fades there (RP rule 7)
      if (k > 0) delete c.audio.fade_in;
      if (k < n - 1) delete c.audio.fade_out;
      parts.push(c);
    }
  });
  const pill = p.text_events.find((e) => e.role === "section_pill")!;
  const pills = Array.from({ length: OVERLAYS }, (_, i) => {
    const len = 30 + ((i * 13) % 150);
    const a = (i * 37) % (total - len);
    return { ...structuredClone(pill), id: `pill-x${i}`, out: [a, a + len], runs: [{ text: `Pill ${i}` }] };
  });
  return JSON.stringify({ ...p, meta: { ...p.meta, title: "R-TL-1 300 parts" }, parts, text_events: [...p.text_events, ...pills] });
}

const stats = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b);
  const q = (f: number) => s[Math.min(s.length - 1, Math.floor(f * s.length))] ?? 0;
  return { n: s.length, p50: q(0.5), p95: q(0.95), p99: q(0.99), max: s[s.length - 1] ?? 0 };
};

test("R-TL-1: pan, zoom, drag at 300 parts / 500 overlays", async ({ browser }) => {
  test.setTimeout(240_000);
  const dir = mkdtempSync(join(tmpdir(), "gmclip-tlperf-"));
  const planPath = join(dir, "tl-300.renderplan.json");
  writeFileSync(planPath, bigPlan());
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    const g = globalThis as unknown as Record<string, unknown>;
    g.__GMCLIP_DEV_BRIDGE__ = info;
    g.__GMCLIP_TL_PERF__ = { draws: [] };
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Home" }).click();
  await importPlanProject(page, `TL perf ${Date.now()}`, planPath);
  await expect(page.getByTestId("cut-title")).toHaveText("R-TL-1 300 parts", { timeout: 90_000 });
  await expect(page.getByRole("list", { name: "Parts" }).getByRole("button")).toHaveCount(PARTS, { timeout: 30_000 });
  await expect(page.getByRole("list", { name: "Overlays" }).getByRole("button")).toHaveCount(OVERLAYS + 11, { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  const lanes = Number(await tl.getAttribute("data-overlay-lanes"));
  const box = (await tl.boundingBox())!;
  const mid = { x: box.x + box.width / 2, y: box.y + 70 };

  // Start recording: every animation frame's interval, and every draw's cost.
  await page.evaluate(() => {
    const g = globalThis as unknown as { __raf: number[]; __rafOn: boolean; __loaf: { duration: number; blocking: number; script: number }[]; __GMCLIP_TL_PERF__: { draws: number[] } };
    g.__GMCLIP_TL_PERF__.draws = [];
    g.__raf = [];
    g.__rafOn = true;
    let last = performance.now();
    const tick = (t: number) => {
      g.__raf.push(t - last);
      last = t;
      if (g.__rafOn) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    // Chrome's long animation frames (≥ 50 ms), with how much of each was script
    g.__loaf = [];
    new PerformanceObserver((l) => {
      for (const e of l.getEntries() as unknown as { duration: number; blockingDuration: number; scripts: { duration: number }[] }[])
        g.__loaf.push({ duration: e.duration, blocking: e.blockingDuration, script: e.scripts.reduce((a, x) => a + x.duration, 0) });
    }).observe({ type: "long-animation-frame", buffered: false });
  });
  const t0 = Date.now();
  // Zoom in to ~40 px/s first (fit is ~24 px/s on this cut), then pan.
  await page.mouse.move(mid.x, mid.y);
  for (let i = 0; i < 4; i++) await page.keyboard.press("=");
  const pan0 = Date.now();
  for (let k = 0; Date.now() - pan0 < 3500; k++) {
    await page.mouse.wheel(k % 80 < 40 ? 60 : -60, 0);
    await page.waitForTimeout(8);
  }
  // Zoom in and out around the pointer (⌥-wheel), across the whole range.
  await page.keyboard.down("Alt");
  const z0 = Date.now();
  for (let k = 0; Date.now() - z0 < 3000; k++) {
    await page.mouse.wheel(0, k % 24 < 12 ? -40 : 40);
    await page.waitForTimeout(16);
  }
  await page.keyboard.up("Alt");
  // (a DOM click: Playwright's own click runs its actionability checks in the page)
  await page.getByRole("button", { name: "Fit" }).evaluate((b: HTMLElement) => b.click());
  for (let i = 0; i < 3; i++) await page.keyboard.press("=");
  // Drags: a part's body (the ghost and its drop slot follow), then a trim edge (⌥:
  // frames only), each cancelled with Esc so nothing is written.
  const d0 = Date.now();
  while (Date.now() - d0 < 3500) {
    await page.mouse.move(mid.x, mid.y);
    await page.mouse.down();
    for (let i = 1; i <= 30; i++) await page.mouse.move(mid.x + Math.sin(i / 5) * 300, mid.y);
    await page.keyboard.press("Escape");
    await page.mouse.up();
  }
  const seconds = (Date.now() - t0) / 1000;
  const r = await page.evaluate(() => {
    const g = globalThis as unknown as { __raf: number[]; __rafOn: boolean; __loaf: { duration: number; blocking: number; script: number }[]; __GMCLIP_TL_PERF__: { draws: number[] } };
    g.__rafOn = false;
    return { raf: g.__raf.slice(1), draws: g.__GMCLIP_TL_PERF__.draws, loaf: g.__loaf };
  });
  const frames = stats(r.raf);
  const draws = stats(r.draws);
  const long = r.raf.filter((x) => x > 50).length;
  const dropped = r.raf.filter((x) => x > (1000 / 60) * 1.5).length;
  // A long frame the page caused: main-thread script or blocking work in it. Chromium's
  // headless frame clock also shows isolated 33–83 ms gaps with no work on the page's
  // main thread at all (seen at the harness's ⌥ release and CDP input bursts): reported,
  // not gated.
  const ours = r.loaf.filter((x) => x.script > 0 || x.blocking > 0);
  console.log(
    `R-TL-1 ${PARTS} parts / ${OVERLAYS + 11} overlays in ${lanes} lanes, ${seconds.toFixed(1)} s: ` +
      `${frames.n} frames, interval p50 ${frames.p50.toFixed(1)} p95 ${frames.p95.toFixed(1)} p99 ${frames.p99.toFixed(1)} max ${frames.max.toFixed(1)} ms; ` +
      `${dropped} over 25 ms, ${long} over 50 ms; long animation frames ${r.loaf.length} (${ours.length} with page work); ` +
      `${draws.n} draws, p50 ${draws.p50.toFixed(2)} p95 ${draws.p95.toFixed(2)} max ${draws.max.toFixed(2)} ms`,
  );
  // nothing was written: still 300 parts
  await expect(page.getByRole("list", { name: "Parts" }).getByRole("button")).toHaveCount(PARTS);
  expect(seconds).toBeGreaterThan(9);
  expect(draws.n).toBeGreaterThan(300);
  expect(draws.p95, "a draw fits well inside a 60 Hz frame").toBeLessThan(8);
  expect(ours, "no long animation frame with work from the page").toEqual([]);
  expect(frames.p99, "99 % of frames on the 60 Hz clock").toBeLessThan(17.5);
  expect(errors).toEqual([]);
});
