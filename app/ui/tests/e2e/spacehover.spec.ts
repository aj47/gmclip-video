// Space from a paused preview plays from the timeline's hover line while the pointer is on
// the timeline, else from the playhead; while playing it pauses (AJ's h79 reel as a video).
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

test("Space plays from the hover line, else from the playhead", async ({ browser }) => {
  test.setTimeout(120_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-space-")), "space-hover.mp4");
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
  await expect(page.getByTestId("cut-title")).toHaveText("space-hover", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const partsY = box.y + Number((await tl.getAttribute("data-lanes-y"))!.split(",")[1]) + 30;
  const x = (f: number) => box.x + 12 + f * ppf;
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  expect((await viewer(page)).frame).toBeLessThan(5);

  // the pointer on the timeline at frame 900 (a hover over the parts, no seek): Space plays there
  await page.mouse.move(x(900), partsY);
  await expect(tl).toHaveAttribute("data-hover-frame", /^(89\d|90\d)$/);
  expect((await viewer(page)).frame, "hovering the parts does not seek").toBeLessThan(5);
  await page.keyboard.press(" ");
  await expect.poll(() => viewer(page), { timeout: 5_000 }).toMatchObject({ playing: true });
  const v1 = await viewer(page);
  expect(v1.frame, `played from the hover line, at ${v1.frame}`).toBeGreaterThanOrEqual(895);
  // playing: Space pauses, wherever the pointer is
  await page.keyboard.press(" ");
  await expect.poll(async () => (await viewer(page)).playing, { timeout: 5_000 }).toBe(false);
  const paused = (await viewer(page)).frame;
  expect(paused).toBeGreaterThanOrEqual(895);

  // the pointer off the timeline: Space plays from the playhead
  await page.mouse.move(box.x + 200, box.y - 200);
  await expect(tl).toHaveAttribute("data-hover-frame", "");
  await page.keyboard.press(" ");
  await expect.poll(() => viewer(page), { timeout: 5_000 }).toMatchObject({ playing: true });
  const v2 = await viewer(page);
  expect(v2.frame - paused, `from the playhead ${paused}, at ${v2.frame}`).toBeLessThan(40);
  expect(v2.frame).toBeGreaterThanOrEqual(paused);
  await page.keyboard.press(" ");
  expect(errors).toEqual([]);
});
