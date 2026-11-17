// H.05: scrubbing the ruler shows the scrub proxy (built in the background when a video
// opens: ≤ 640 px, GOP 4), or the master's keyframes before the proxy reaches the
// playhead; once the pointer rests the viewer shows the exact frame from the master.
// Playback never reads the proxy. Then (AJ 2026-10-15: "i should be able to toggle scrub
// on hover timeline") hover scrub is a toggle (⌥S, View › Hover Scrub, the palette; the
// timeline toolbar's own went in decision 0054), saved in the app's UI preferences (ui-prefs.json, not the profile). On:
// hovering the ruler moves the viewer. Off: hovering shows a hover line and its time
// only; the viewer stays where it was.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labMedia, labPlan } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");
const H79 = labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json"));

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

test("scrub from the proxy, settle on the exact frame", async ({ browser }) => {
  test.skip(!existsSync(VIDEO), "no test video on this machine");
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByTestId("open-video")).toBeVisible();
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  const tc = page.getByTestId("timecode");
  const box = (await tl.boundingBox())!;
  const rul = (await page.getByTestId("timeline-ruler").boundingBox())!; // the pinned ruler
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const xAt = (f: number) => box.x + 12 + f * ppf;

  // Hover-scrub the ruler until the proxy covers it (a minute of source builds in
  // seconds); before that the frames are the master's keyframes.
  const seen = new Set<string>();
  let f = 100;
  await expect
    .poll(
      async () => {
        f = f >= 1500 ? 100 : f + 37;
        await page.mouse.move(xAt(f), rul.y + 6);
        await page.waitForTimeout(60);
        const p = (await tc.getAttribute("data-scrub-path")) ?? "";
        if (p) seen.add(p);
        return p;
      },
      { timeout: 120_000, intervals: [50] },
    )
    .toBe("proxy");
  for (const p of seen) expect(["proxy", "keyframe", "exact"]).toContain(p);
  await expect(page.getByTestId("scrub-source")).toHaveText("scrub preview");

  // A drag across the ruler: every step a proxy frame, the viewer following.
  await page.mouse.move(xAt(200), rul.y + 6);
  await page.mouse.down();
  for (let k = 1; k <= 20; k++) await page.mouse.move(xAt(200 + k * 50), rul.y + 6);
  await expect(tc).toHaveAttribute("data-scrub-path", "proxy");
  await page.mouse.up();
  // At rest: the exact frame, where the pointer stopped.
  await expect(tc).toHaveAttribute("data-scrub-path", "exact", { timeout: 5_000 });
  const at = Number(await tc.getAttribute("data-frame"));
  expect(Math.abs(at - 1200)).toBeLessThanOrEqual(3);
  await expect(page.getByTestId("scrub-source")).toHaveCount(0);

  // Playback: exact frames only.
  await page.mouse.move(box.x + box.width / 2, box.y - 200);
  await page.getByRole("button", { name: "Play", exact: true }).click();
  await page.waitForTimeout(800);
  await expect(tc).toHaveAttribute("data-scrub-path", "exact");
  await page.getByRole("button", { name: "Pause", exact: true }).click();
  expect(errors).toEqual([]);
});

const frameOf = async (page: Page) => Number((await page.getByTestId("timecode").getAttribute("data-frame")) ?? NaN);
const bridge = (page: Page, command: string, args: Record<string, unknown> = {}) =>
  page.evaluate(
    ([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: unknown) => Promise<{ output: Record<string, unknown> }> }).__gmclipBridgeCall(c as string, a),
    [command, args] as const,
  );

test("hover scrub toggles, and the choice is saved", async ({ browser }) => {
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await importPlanProject(page, `Hover ${Date.now()}`, H79);
  await expect(page.getByTestId("cut-title")).toHaveText("My PC made this in 67 seconds", { timeout: 60_000 });

  const tl = page.locator(".timeline-canvas");
  const scrubbing = (on: boolean) => expect(tl).toHaveAttribute("data-hover-scrub", on ? "1" : "0");
  const box = (await tl.boundingBox())!;
  const rul = (await page.getByTestId("timeline-ruler").boundingBox())!; // the pinned ruler
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const xAt = (f: number) => box.x + 12 + f * ppf;

  // On by default: hovering the ruler moves the viewer.
  await scrubbing(true);
  await page.mouse.move(xAt(850), rul.y + 6);
  await page.mouse.move(xAt(900), rul.y + 6, { steps: 3 });
  await expect.poll(() => frameOf(page), { timeout: 5_000 }).toBeGreaterThan(850);
  const held = await frameOf(page);

  // Off (View › Hover scrub): the hover line follows, the viewer does not.
  await page.getByTestId("view-menu").click();
  await expect(page.getByTestId("view-hover-scrub")).toHaveAttribute("aria-checked", "true");
  await page.getByTestId("view-hover-scrub").click();
  await scrubbing(false);
  await page.mouse.move(xAt(350), rul.y + 6);
  await page.mouse.move(xAt(300), rul.y + 6, { steps: 3 });
  await expect.poll(async () => Number(await tl.getAttribute("data-hover-frame")), { timeout: 5_000 }).toBeLessThan(320);
  await page.waitForTimeout(400);
  expect(await frameOf(page)).toBe(held);
  // saved in the app's UI preferences
  await expect.poll(async () => (await bridge(page, "ui_prefs_get")).output.hover_scrub).toBe(false);

  // ⌥S turns it back on (the pointer away from any control).
  await page.mouse.move(box.x + box.width / 2, box.y - 200);
  await page.keyboard.press("Alt+KeyS");
  await scrubbing(true);
  await expect.poll(async () => (await bridge(page, "ui_prefs_get")).output.hover_scrub).toBe(true);
  await page.mouse.move(xAt(500), rul.y + 6);
  await page.mouse.move(xAt(520), rul.y + 6, { steps: 3 });
  await expect.poll(() => frameOf(page), { timeout: 5_000 }).toBeLessThan(600);

  // View › Hover Scrub (the native menu sends the same event as app_menu).
  await page.waitForTimeout(500);
  await bridge(page, "app_menu", { id: "hover_scrub" });
  await scrubbing(false);
  // The ? list documents the key.
  await page.mouse.move(box.x + box.width / 2, box.y - 200);
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toContainText("Hover scrub on / off");
  await page.keyboard.press("Escape");
  // leave it on for the specs that follow in this launch
  await bridge(page, "ui_prefs_set", { hover_scrub: true });
  expect(errors).toEqual([]);
});
