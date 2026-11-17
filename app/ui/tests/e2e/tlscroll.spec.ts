// Moving through time without a sideways wheel: the plain wheel over the parts scrolls
// time, ⇧-wheel too, a middle-button drag pans, and the scrollbar under the lanes drags
// and jumps on a click on its track.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("wheel, ⇧-wheel, middle drag and the scrollbar move through time", async ({ browser }) => {
  test.setTimeout(120_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-tlscroll-")), "tlscroll-test.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  // a short window, so the lanes also scroll up and down
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("tlscroll-test", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  await expect.poll(async () => Number(await tl.getAttribute("data-frames"))).toBeGreaterThan(0);
  const scroll = async () => Number(await tl.getAttribute("data-scroll"));
  const bar = page.getByTestId("timeline-hscroll");

  // fit: nothing to scroll, no thumb
  await expect(bar.locator(".tl-hscroll-thumb")).toHaveCount(0);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  for (let k = 0; k < 6; k++) await page.keyboard.press("=");
  await expect(bar.locator(".tl-hscroll-thumb")).toHaveCount(1);
  const box = (await tl.boundingBox())!;
  const rul = (await page.getByTestId("timeline-ruler").boundingBox())!; // the pinned ruler
  const partsY = box.y + Number((await tl.getAttribute("data-lanes-y"))!.split(",")[1]) + 20;

  // the plain wheel over the parts: time, never the lanes
  await page.mouse.click(box.x + 4, rul.y + 4); // focus without moving the view much
  await page.evaluate(() => { document.querySelector(".timeline-body")!.scrollTop = 0; });
  await page.mouse.move(box.x + 300, partsY);
  let before = await scroll();
  await page.mouse.wheel(0, 600);
  await expect.poll(scroll).toBeGreaterThan(before);
  expect(await page.evaluate(() => document.querySelector(".timeline-body")!.scrollTop)).toBe(0);

  // ⇧-wheel: time (as deltaY, or as the deltaX macOS makes of it)
  before = await scroll();
  await page.keyboard.down("Shift");
  await page.mouse.wheel(0, 400);
  await page.keyboard.up("Shift");
  await expect.poll(scroll).toBeGreaterThan(before);

  // a middle-button drag pans: dragging left moves later in time
  before = await scroll();
  await page.mouse.move(box.x + 600, partsY);
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(box.x + 400, partsY, { steps: 5 });
  await page.mouse.up({ button: "middle" });
  await expect.poll(scroll).toBeCloseTo(before + 200, 0);

  // the scrollbar: a click near the track's start jumps there, dragging the thumb moves on
  const tb = (await bar.boundingBox())!;
  await page.mouse.click(tb.x + 2, tb.y + tb.height / 2);
  await expect.poll(scroll).toBe(0);
  const th = (await bar.locator(".tl-hscroll-thumb").boundingBox())!;
  await page.mouse.move(th.x + th.width / 2, th.y + th.height / 2);
  await page.mouse.down();
  await page.mouse.move(th.x + th.width / 2 + 100, th.y + th.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect.poll(scroll).toBeGreaterThan(0);
  expect(errors).toEqual([]);
});
