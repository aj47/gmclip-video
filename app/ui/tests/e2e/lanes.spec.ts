// U.08 remainder on a model cut (AJ's h79 reel opened as a video): drag the text hook on
// its overlay lane (move, then trim its end), each drag one undo step; Esc cancels a drag
// with nothing written; the gain lane takes a double-click (a segment at that level; a
// single click adds nothing), a drag
// (moved and lowered), and a double-click (removed); `m` / `⇧m` measure from the playhead
// and the bracket's value lists with two evidence frames. (An imported RenderPlan's
// read-only lanes: timeline.spec.)
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

async function open(browser: import("@playwright/test").Browser): Promise<{ page: Page; errors: string[] }> {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  return { page, errors };
}

/** The timeline's geometry, from its data attributes. */
/** The timeline's geometry once it has settled (two reads 150 ms apart agree): after an
 *  undo the canvas re-lays out, and a drag from the old geometry lands elsewhere. */
async function settledGeom(page: Page) {
  let last = "";
  for (let i = 0; i < 40; i++) {
    const g = await geom(page);
    const key = JSON.stringify([g.box, g.ppf, g.measY, g.partsY, g.gainY, g.capY, g.ovY, g.ovStep, g.x(0)]);
    if (key === last) return g;
    last = key;
    await page.waitForTimeout(150);
  }
  return geom(page);
}

async function geom(page: Page) {
  const tl = page.locator(".timeline-canvas");
  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const scroll = Number(await tl.getAttribute("data-scroll"));
  const [measY, partsY, gainY, capY, ovY, ovStep] = (await tl.getAttribute("data-lanes-y"))!.split(",").map(Number) as [number, number, number, number, number, number];
  return { tl, box, ppf, x: (f: number) => box.x + 12 + f * ppf - scroll, measY, partsY, gainY, capY, ovY, ovStep };
}

test("overlay drags, gain lane, measure brackets", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-lanes-")), "lanes-test.mp4");
  copyFileSync(VIDEO, copy);
  const { page, errors } = await open(browser);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("lanes-test", { timeout: 30_000 });
  await openTab(page, "Short");
  await page.getByLabel("Hook text").fill("My PC made this");
  await page.getByLabel("Hook seconds").fill("3");
  await page.getByRole("button", { name: "Set hook" }).click();
  const tl = page.locator(".timeline-canvas");
  await expect(tl).toHaveAttribute("data-overlays-editable", "1", { timeout: 20_000 });
  const hookRow = page.getByRole("list", { name: "Overlays" }).getByRole("button").filter({ hasText: "text_hook" });
  await expect(hookRow).toContainText("frames 0–90, lane 1", { timeout: 20_000 });

  // Drag the hook's body 60 frames right (⌥: no snapping): one undo step.
  let g = await geom(page);
  const oy = g.box.y + g.ovY + 8;
  await page.keyboard.down("Alt");
  await page.mouse.move(g.x(45), oy);
  await page.mouse.down();
  await page.mouse.move(g.x(75), oy, { steps: 4 });
  await page.mouse.move(g.x(105), oy, { steps: 4 });
  await expect(tl).toHaveAttribute("data-dragging", "lane");
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(hookRow).toContainText("frames 60–150", { timeout: 20_000 });
  await expect(tl).toHaveAttribute("data-dragging", "");
  // ⌘Z: back in one step
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(hookRow).toContainText("frames 0–90", { timeout: 20_000 });
  await expect(tl).toHaveAttribute("data-dragging", "");

  // Trim its end to frame 120 (⌥ drag of the out edge).
  g = await settledGeom(page);
  await page.keyboard.down("Alt");
  await page.mouse.move(g.x(90) - 2, oy);
  await page.mouse.down();
  await page.mouse.move(g.x(120) - 2, oy, { steps: 6 });
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(hookRow).toContainText("frames 0–120", { timeout: 20_000 });

  // Esc during a drag: nothing is written.
  await page.mouse.move(g.x(40), oy);
  await page.mouse.down();
  await page.mouse.move(g.x(200), oy, { steps: 6 });
  await page.keyboard.press("Escape");
  await page.mouse.up();
  await expect(tl).toHaveAttribute("data-dragging", "");
  await page.waitForTimeout(500);
  await expect(hookRow).toContainText("frames 0–120");

  // The gain lane: a single click adds nothing; a double-click at frame 300, +12 dB → a
  // 1 s segment there.
  const gainRows = page.getByRole("list", { name: "Gain" }).getByRole("button");
  const half = 15; // GAIN_H / 2
  const yDb = (db: number) => g.box.y + g.gainY + half - (db / 24) * (half - 2);
  await page.mouse.click(g.x(300), yDb(12));
  await page.waitForTimeout(600);
  await expect(gainRows).toHaveCount(0);
  await page.mouse.dblclick(g.x(300), yDb(12));
  await expect(gainRows).toHaveCount(1, { timeout: 20_000 });
  await expect(gainRows.first()).toContainText("+12 dB (frames 285–315)");
  // drag its body 30 frames right and down to +6 dB, one undo step
  await page.keyboard.down("Alt");
  await page.mouse.move(g.x(300), yDb(12));
  await page.mouse.down();
  await page.mouse.move(g.x(315), yDb(9), { steps: 4 });
  await page.mouse.move(g.x(330), yDb(6), { steps: 4 });
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(gainRows.first()).toContainText("+6 dB (frames 315–345)", { timeout: 20_000 });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(gainRows.first()).toContainText("+12 dB (frames 285–315)", { timeout: 20_000 });
  // its out edge to 360
  await page.keyboard.down("Alt");
  await page.mouse.move(g.x(315) - 2, yDb(12));
  await page.mouse.down();
  await page.mouse.move(g.x(360) - 2, yDb(12), { steps: 6 });
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(gainRows.first()).toContainText("frames 285–360", { timeout: 20_000 });
  // double-click removes it
  await page.mouse.dblclick(g.x(320), yDb(12));
  await expect(gainRows).toHaveCount(0, { timeout: 20_000 });

  // m / ⇧m: a measurement from frame 30 to frame 90 (2 s) with its bracket.
  await page.locator(".cut-head").hover();
  const tc = page.getByTestId("timecode");
  await page.keyboard.press("Home");
  await expect(tc).toHaveText(/^0:00\.00 /);
  for (let i = 0; i < 3; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(tc).toHaveText(/^0:01\.00 /);
  await page.keyboard.press("m");
  await expect(tl).toHaveAttribute("data-measuring", "30");
  for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(tc).toHaveText(/^0:03\.00 /);
  await page.keyboard.press("Shift+M");
  const meas = page.getByRole("list", { name: "Measurements" }).getByRole("button");
  await expect(meas.first()).toContainText("2.00 s (frames 30–90, 2 evidence frames)", { timeout: 20_000 });
  await expect(tl).toHaveAttribute("data-measuring", "");
  expect(errors).toEqual([]);
});
