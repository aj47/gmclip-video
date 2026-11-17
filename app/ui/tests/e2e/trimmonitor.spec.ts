// Decision 0053, the trim monitor, on AJ's h79 reel opened as a video and bladed in two:
// dragging part 1's out edge shows "+8f" by the edge and the viewer, paused, on the frame
// the trim would end the part on (the cut as it would be: 8 frames longer); back past where
// it started, "−5f" and the frame 5 before the old end. On release the trim commits and the
// viewer is back on the frame it showed before the drag.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;
type Viewer = { frame: number; frames: number; playing: boolean };
const viewer = async (page: Page): Promise<Viewer> => {
  const r = await page.evaluate(() => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall("viewer_state", {}));
  if (!r.ok) throw new Error(JSON.stringify(r.error));
  return r.output as Viewer;
};

async function geom(page: Page) {
  const tl = page.locator(".timeline-canvas");
  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const scroll = Number(await tl.getAttribute("data-scroll"));
  const partsY = Number((await tl.getAttribute("data-lanes-y"))!.split(",")[1]);
  const ruler = (await page.getByTestId("timeline-ruler").boundingBox())!;
  return { box, ruler, ppf, x: (f: number) => box.x + 12 + f * ppf - scroll, y: box.y + partsY + 30 };
}

const partRows = (page: Page) => page.getByRole("list", { name: "Parts" }).getByRole("button");
const frames = async (page: Page) =>
  (await partRows(page).allTextContents()).map((t) => /frames (\d+)–(\d+)/.exec(t)!.slice(1).map(Number) as [number, number]);

test("a part edge drag shows its frame and ±frames; release goes back", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-trimmon-")), "trim-monitor.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("trim-monitor", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  const total = Number(await tl.getAttribute("data-frames"));

  // blade in two near the middle
  await page.getByRole("button", { name: "Blade tool" }).click();
  let g = await geom(page);
  await page.mouse.click(g.x(Math.round(total / 2)), g.y);
  await expect(partRows(page)).toHaveCount(2, { timeout: 20_000 });
  await page.getByRole("button", { name: "Select tool" }).click();
  const end = (await frames(page))[0]![1];

  // the playhead somewhere else first: where the viewer comes back to
  g = await geom(page);
  await page.mouse.click(g.x(200), g.ruler.y + 10);
  await expect.poll(async () => (await viewer(page)).frame, { timeout: 10_000 }).toBeGreaterThan(150);
  await expect.poll(async () => (await viewer(page)).frame, { timeout: 10_000 }).toBeLessThan(250);
  const before = (await viewer(page)).frame;

  // grab part 1's out edge (2 px inside it); ⌥ holds snapping off
  g = await geom(page);
  await page.mouse.move(g.x(end) - 2, g.y);
  await page.mouse.down();
  await page.keyboard.down("Alt");
  await page.mouse.move(g.x(end + 8) - 2, g.y, { steps: 6 });
  const readout = page.getByTestId("trim-readout");
  await expect(readout).toHaveText("+8f");
  await expect(readout).toHaveAttribute("data-delta", "8");
  // the cut as the trim would leave it, paused on the part's new last frame
  await expect.poll(() => viewer(page), { timeout: 10_000 }).toMatchObject({ frame: end + 7, frames: total + 8, playing: false });

  await page.mouse.move(g.x(end - 5) - 2, g.y, { steps: 6 });
  await expect(readout).toHaveText("−5f");
  await expect.poll(() => viewer(page), { timeout: 10_000 }).toMatchObject({ frame: end - 6, frames: total - 5 });

  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(readout).toHaveCount(0);
  await expect.poll(async () => (await frames(page))[0]![1], { timeout: 20_000 }).toBe(end - 5);
  // back where it was, on the trimmed cut
  await expect.poll(() => viewer(page), { timeout: 10_000 }).toMatchObject({ frame: before, frames: total - 5 });

  // Esc mid-drag: nothing written, the viewer back where it was
  g = await geom(page);
  await page.mouse.move(g.x(end - 5) - 2, g.y);
  await page.mouse.down();
  await page.keyboard.down("Alt");
  await page.mouse.move(g.x(end + 10) - 2, g.y, { steps: 6 });
  await expect(readout).toHaveText("+15f");
  await expect.poll(async () => (await viewer(page)).frames, { timeout: 10_000 }).toBe(total + 10);
  await page.keyboard.press("Escape");
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(readout).toHaveCount(0);
  await expect.poll(() => viewer(page), { timeout: 10_000 }).toMatchObject({ frame: before, frames: total - 5 });
  expect((await frames(page))[0]![1]).toBe(end - 5);
  expect(errors).toEqual([]);
});
