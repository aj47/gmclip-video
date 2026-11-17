// R-EVT-1 [M2], UI side (SPEC §13; backlog A2.15 / U.18): a 1 000-op agent burst keeps
// the page's input latency within one frame. h79's plan is imported as a project and its
// cut opened; `dev_agent_burst` (dev bridge only) then sends 1 000 agent edits from
// another thread, ten times, while the test presses keys the page handles itself
// (timeline zoom in / out). Gated: each key's wait for the main thread (its time stamp to
// the first listener): never a frame, p99 ≤ half a frame. Reported: Event Timing's keys
// painted later than the next frame, input → next animation frame, the same keys with no
// burst, and the project scope's event batches (the transport coalesces per 16 ms).
// Chromium against the dev bridge; WKWebView and the Air are AJ.02.
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labPlan } from "./lab";

const H79 = labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json"));
const OPS = 1000;
const ROUNDS = 10;
const FRAME_MS = 1000 / 60;

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

type Reply = { ok: boolean; output?: unknown; error?: unknown };
type Timing = { duration: number; delay: number; work: number };
type G = {
  __evt: { batches: number; events: number };
  __lat: number[];
  __wait: number[];
  __timing: Timing[];
  __keys: number;
  __measure: boolean;
};

const stats = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b);
  const q = (f: number) => s[Math.min(s.length - 1, Math.floor(f * s.length))] ?? 0;
  return { n: s.length, p50: q(0.5), p95: q(0.95), p99: q(0.99), max: s[s.length - 1] ?? 0 };
};

test("R-EVT-1: a 1 000-op agent burst keeps input within a frame", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    const g = globalThis as unknown as Record<string, unknown> & G;
    g.__GMCLIP_DEV_BRIDGE__ = info;
    g.__evt = { batches: 0, events: 0 };
    g.__lat = [];
    g.__wait = [];
    g.__timing = [];
    g.__keys = 0;
    g.__measure = false;
    // Event Timing: each keydown's input → next paint (`duration`, 8 ms grain), the time
    // it waited for the main thread (`delay`) and its handlers' time (`work`). Only
    // events of 16 ms or more are reported; the rest were shown within a frame.
    new PerformanceObserver((l) => {
      if (!g.__measure) return;
      for (const e of l.getEntries() as unknown as { name: string; duration: number; startTime: number; processingStart: number; processingEnd: number }[])
        if (e.name === "keydown") g.__timing.push({ duration: e.duration, delay: e.processingStart - e.startTime, work: e.processingEnd - e.processingStart });
    }).observe({ type: "event", durationThreshold: 16, buffered: false } as PerformanceObserverInit);
    // count the bus's event batches as the bridge delivers them
    const d = Object.getOwnPropertyDescriptor(WebSocket.prototype, "onmessage")!;
    Object.defineProperty(WebSocket.prototype, "onmessage", {
      configurable: true,
      get() {
        return d.get!.call(this);
      },
      set(fn: (m: MessageEvent) => void) {
        d.set!.call(this, (m: MessageEvent) => {
          if (typeof m.data === "string" && m.data.includes('"event"')) {
            try {
              const ev = (JSON.parse(m.data) as { event?: { scope: string; events: unknown[] } }).event;
              // the project's own scope: the one the agent's edits are published on
              if (ev?.scope.startsWith("project:")) {
                g.__evt.batches += 1;
                g.__evt.events += ev.events.length;
              }
            } catch {
              /* not ours */
            }
          }
          return fn.call(this, m);
        });
      },
    });
    // input → the next frame the page can draw, per key
    addEventListener(
      "keydown",
      (e) => {
        if (!g.__measure) return;
        g.__keys += 1;
        const t0 = e.timeStamp;
        // how long the key waited for the main thread (the first listener it reaches)
        g.__wait.push(performance.now() - t0);
        requestAnimationFrame(() => g.__lat.push(performance.now() - t0));
      },
      { capture: true },
    );
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Home" }).click();
  await importPlanProject(page, `R-EVT-1 ${Date.now()}`, H79);
  await expect(page.getByTestId("cut-title")).toHaveText("My PC made this in 67 seconds", { timeout: 90_000 });
  await expect(page.getByRole("list", { name: "Parts" }).getByRole("button")).toHaveCount(14, { timeout: 30_000 });

  const call = async <T,>(command: string, args: Record<string, unknown>, project?: string): Promise<T> => {
    const r = await page.evaluate(
      ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: unknown, p?: string) => Promise<Reply> }).__gmclipBridgeCall(c as string, a, p as string | undefined),
      [command, args, project] as const,
    );
    if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
    return r.output as T;
  };
  const open = await call<{ id: string; name: string; cuts: { id: string }[] }[]>("project_list_open", {});
  const proj = open.find((p) => p.name.startsWith("R-EVT-1"))!;
  const cut = proj.cuts[0]!.id;
  const view = await call<{ parts: { id: string }[] }>("cut_view", { cut }, proj.id);
  const part = view.parts[5]!.id;

  // keys the page handles itself: timeline zoom in and out, alternately
  const tl = page.locator(".timeline-canvas");
  const box = (await tl.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 40);
  const press = async (k: number) => {
    await page.keyboard.press(k % 2 ? "-" : "=");
    await page.waitForTimeout(12);
  };

  type Run = { lat: number[]; wait: number[]; timing: Timing[]; keys: number };
  const take = () =>
    page.evaluate(() => {
      const g = globalThis as unknown as G;
      const r = { lat: g.__lat, wait: g.__wait, timing: g.__timing, keys: g.__keys };
      g.__lat = [];
      g.__wait = [];
      g.__timing = [];
      g.__keys = 0;
      return r;
    });
  // let the import's own after-work settle (its analysis, the columns, the first plan),
  // with a few unmeasured keys to warm the handlers
  await page.waitForTimeout(2000);
  for (let k = 0; k < 40; k++) await press(k);
  await page.evaluate(() => ((globalThis as unknown as G).__measure = true));
  // ROUNDS bursts of OPS agent edits, keys pressed all the while
  type St = { running: boolean; ok: number; refused: number; first_refusal: string | null; ms: number };
  const before = await page.evaluate(() => ({ ...(globalThis as unknown as G).__evt }));
  let ok = 0;
  let ms = 0;
  let k = 0;
  for (let round = 0; round < ROUNDS; round++) {
    await call("dev_agent_burst", { cut, part, n: OPS }, proj.id);
    let st = await call<St>("dev_agent_burst", { status: true }, proj.id);
    const t0 = Date.now();
    // at least six keys per burst: the bursts got fast enough (~60 ms) that a round could
    // finish before enough keys landed to measure anything
    let pressed = 0;
    while ((st.running || pressed < 6) && Date.now() - t0 < 120_000) {
      for (let i = 0; i < 3; i++) await press(k++);
      pressed += 3;
      st = await call<St>("dev_agent_burst", { status: true }, proj.id);
    }
    expect(st.running).toBe(false);
    expect(st.ok, `${st.first_refusal}`).toBe(OPS);
    ok += st.ok;
    ms += st.ms;
  }
  // the batches still in flight when the last op landed
  await page.waitForTimeout(250);
  const during: Run = await take();
  // the same keys with no burst, after it
  for (let k = 0; k < 80; k++) await press(k);
  await page.waitForTimeout(100);
  const base: Run = await take();

  const evt = await page.evaluate(() => {
    const g = globalThis as unknown as G;
    g.__measure = false;
    return { ...g.__evt };
  });
  const batches = evt.batches - before.batches;
  const events = evt.events - before.events;
  const lat = stats(during.lat);
  const b = stats(base.lat);
  // Event Timing: keys shown later than the frame after them (8 ms grain: ≥ 24 ms)
  const late = (r: Run) => r.timing.filter((t) => t.duration > FRAME_MS + 4);
  const wait = stats(during.wait);
  const bwait = stats(base.wait);
  console.log(
    `R-EVT-1 UI: ${ok} agent ops in ${ROUNDS} bursts, ${ms.toFixed(0)} ms in all; ${during.keys} keys pressed while they landed: ` +
      `input → next frame p50 ${lat.p50.toFixed(1)} p95 ${lat.p95.toFixed(1)} p99 ${lat.p99.toFixed(1)} max ${lat.max.toFixed(1)} ms; ` +
      `Event Timing: ${during.timing.length} keys ≥ 16 ms to paint, ${late(during).length} later than a frame (${late(during).map((t) => t.duration).join(", ")} ms), ` +
      `wait for the main thread p50 ${wait.p50.toFixed(1)} p99 ${wait.p99.toFixed(1)} max ${wait.max.toFixed(1)} ms. ` +
      `No burst (${base.keys} keys): next frame p50 ${b.p50.toFixed(1)} p99 ${b.p99.toFixed(1)} ms, wait p99 ${bwait.p99.toFixed(1)} ms, ` +
      `${late(base).length} later than a frame. Bus (project scope): ${batches} batches carrying ${events} events`,
  );
  expect(ok).toBe(OPS * ROUNDS);
  expect(during.keys).toBeGreaterThan(40);
  // R-EVT-1: the page takes every key within a frame while the bursts land — no key
  // waits a frame for the main thread, and the p99 wait stays under half a frame, which
  // leaves the rest of the frame to show it. (Before the store refreshed the cut one at a
  // time a key waited up to 149 ms.) Event Timing's late paints are reported, not gated:
  // headless Chromium's 60 Hz clock paints keys late with the main thread idle (13 of 70
  // in one run with a 1.2 ms worst wait), and with no burst at all.
  expect(wait.max, "the longest wait for the main thread").toBeLessThanOrEqual(FRAME_MS);
  expect(wait.p99, "a key's wait for the main thread, p99").toBeLessThanOrEqual(FRAME_MS / 2);
  // coalesced: at most about one batch per 16 ms frame, far fewer than one per op
  expect(batches).toBeLessThan((OPS * ROUNDS) / 10);
  expect(batches).toBeLessThanOrEqual(Math.ceil(ms / 16) + 2 * ROUNDS + 20);
  expect(errors).toEqual([]);
});
