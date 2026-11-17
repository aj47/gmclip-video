// U.06 on a long real transcript (R-WORD-1 in the app): AJ's 2 h 40 min AMA stream
// (loc_54b47e51, 15.9k words) opened from its project fixture, its file
// found (the 48 kHz proxy: the YouTube previews of the 3 h streams are 44.1 kHz, which
// the planner does not play yet), and its stream edit opened: the words strip holds every
// word of the stream. Gates: 10 s of wheel scrolling through the strip has no long
// animation frame (≥ 50 ms, Chrome's LoAF) with work from the page; a strike from the
// strip lands as one undo step, shows on the word ≤ 100 ms after the key and the cut's
// new length ≤ 1 s after (the command, the cut re-read, the strip redrawn); strike and restore each cost ≤ 16 ms p50 over the bridge
// (the app command and the core). Numbers
// are printed for the backlog. The core-only bench on a 3 h transcript is A2.04's.
import { expect, test, type Page } from "@playwright/test";
import { existsSync } from "node:fs";
import { openPage } from "./ux-lib";
import { labMedia, openLabProject } from "./lab";

const PROXY = labMedia("preview/loc_54b47e51.proxy.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(PROXY), "no long stream in the lab media on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;
async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project = ""): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

interface Loaf {
  duration: number;
  blocking: number;
  script: number;
}

test("words strip on a 2 h 40 min transcript", async ({ browser }) => {
  test.setTimeout(300_000);
  let { page } = await openPage(browser, { width: 1440, height: 900 });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await openLabProject(page, "loc_54b47e51");
  const pid = (await bridge<{ id: string; name: string }[]>(page, "project_list_open", {}))[0]!.id;
  const src = (await bridge<{ sources: { id: string }[] }>(page, "source_list", {}, pid)).sources[0]!;
  await bridge(page, "video_relink", { source: src.id, path: PROXY }, pid);
  const se = await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid);
  // the stream edit shown as the stream is: a 16:9 framing
  await bridge(page, "cut_output_set", { cut: se.cut, aspect: "16:9" }, pid);
  const title = (await bridge<{ id: string; cuts: { id: string; title: string }[] }[]>(page, "project_list_open", {}))[0]!.cuts.find((c) => c.id === se.cut)!.title;
  await page.getByRole("button", { name: "Home" }).click();
  console.log(`stream edit: “${title}”`);
  await page.locator('.project .item[data-framed="yes"]').filter({ hasText: title }).first().getByRole("button", { name: "Open" }).click();
  await expect(page.getByTestId("cut-title")).toHaveText(title, { timeout: 30_000 });

  const strip = page.getByRole("listbox", { name: "Cut words" });
  await expect(strip).toBeVisible({ timeout: 60_000 });
  const total = await strip.getAttribute("data-words");
  console.log(`words strip: ${total} words`);
  expect(Number(total)).toBeGreaterThan(15_000);

  // 10 s of scrolling, half of it far down the stream
  await page.evaluate(() => {
    const g = globalThis as unknown as { __loaf: Loaf[]; __frames: number[] };
    g.__loaf = [];
    g.__frames = [];
    new PerformanceObserver((l) => {
      for (const e of l.getEntries() as unknown as { duration: number; blockingDuration: number; scripts: { duration: number }[] }[])
        g.__loaf.push({ duration: e.duration, blocking: e.blockingDuration, script: e.scripts.reduce((a, x) => a + x.duration, 0) });
    }).observe({ type: "long-animation-frame", buffered: false });
    let last = performance.now();
    const tick = (t: number) => {
      g.__frames.push(t - last);
      last = t;
      if (g.__frames.length < 2000) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  const sb = (await strip.boundingBox())!;
  await page.mouse.move(sb.x + sb.width / 2, sb.y + sb.height / 2);
  await strip.evaluate((el) => {
    const g = globalThis as unknown as { __deepest: number };
    g.__deepest = 0;
    el.addEventListener("scroll", () => (g.__deepest = Math.max(g.__deepest, el.scrollTop)), { passive: true });
  });
  const t0 = Date.now();
  for (let k = 0; Date.now() - t0 < 10_000; k++) {
    // down for 6 s (deep into the stream), then back up
    await page.mouse.wheel(0, Date.now() - t0 < 6_000 ? 400 : -300);
    await page.waitForTimeout(16);
  }
  const deep = await page.evaluate(() => (globalThis as unknown as { __deepest: number }).__deepest);
  const height = await strip.evaluate((el) => el.scrollHeight);
  const perf = await page.evaluate(() => {
    const g = globalThis as unknown as { __loaf: Loaf[]; __frames: number[] };
    const f = [...g.__frames].sort((a, b) => a - b);
    return { loaf: g.__loaf, frames: f.length, p99: f[Math.floor(f.length * 0.99)] ?? 0, max: f[f.length - 1] ?? 0 };
  });
  console.log(`scroll: ${perf.frames} frames, p99 ${perf.p99.toFixed(1)} ms, max ${perf.max.toFixed(1)} ms, long frames ${perf.loaf.length} ${JSON.stringify(perf.loaf.slice(0, 5))}; scrolled to ${deep} of ${height} px`);
  expect(deep).toBeGreaterThan(height * 0.5);
  expect(perf.loaf.filter((l) => l.script > 0)).toEqual([]);

  // strike a word deep in the stream with d, then restore it: one undo step each
  const w = strip.locator(".w:not(.struck)").nth(40);
  await w.scrollIntoViewIfNeeded();
  const i = await w.getAttribute("data-i");
  const rev = async () => (await bridge<{ rev: number }>(page, "project_save_state", {}, pid)).rev;
  const r0 = await rev();
  await w.click();
  // shown: the word struck on screen; settled: the cut's length re-read from the core
  const { shown, settled } = await page.evaluate(async (wi) => {
    const strip = document.querySelector<HTMLElement>("[data-testid=words]")!;
    const meta = document.querySelector<HTMLElement>("[data-testid=cut-meta]")!;
    const s0 = meta.dataset.seconds;
    const t = performance.now();
    let shown = -1;
    strip.dispatchEvent(new KeyboardEvent("keydown", { key: "d", bubbles: true }));
    for (;;) {
      await new Promise((r) => requestAnimationFrame(r));
      if (shown < 0 && strip.querySelector(`.w.struck[data-i="${wi}"]`)) shown = performance.now() - t;
      if (meta.dataset.seconds !== s0) return { shown, settled: performance.now() - t };
      if (performance.now() - t > 5000) return { shown, settled: -1 };
    }
  }, i);
  expect(await rev()).toBe(r0 + 1);
  expect(errors).toEqual([]);
  // the round trip of the same command (the strip's own call), restore and strike, from
  // a page that is not also re-reading the cut after each edit
  await page.close();
  ({ page } = await openPage(browser, { width: 1100, height: 700 }));
  const trips: number[] = [];
  for (let k = 0; k < 10; k++) {
    for (const cmd of ["words_restore_ord", "words_strike_ord"]) {
      trips.push(await page.evaluate(async ([c, cut, p, n]) => {
        const t = performance.now();
        const r = await (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, { cut, from: n, to: n }, p);
        if (!r.ok) throw new Error(JSON.stringify(r.error));
        return performance.now() - t;
      }, [cmd, se.cut, pid, Number(i)] as [string, string, string, number]));
    }
  }
  trips.sort((a, b) => a - b);
  const p50 = trips[Math.floor(trips.length / 2)]!;
  console.log(`strike shown after ${shown.toFixed(0)} ms, the cut re-read after ${settled.toFixed(0)} ms; strike/restore over the bridge p50 ${p50.toFixed(1)} ms, max ${trips[trips.length - 1]!.toFixed(1)} ms`);
  expect(shown).toBeGreaterThan(0);
  expect(shown).toBeLessThan(100);
  expect(settled).toBeGreaterThan(0);
  expect(settled).toBeLessThan(1000);
  expect(p50).toBeLessThanOrEqual(16);
  expect(await rev()).toBe(r0 + 21);
  await page.close();
});
