// U.08 gaps (SPEC §5.3) on AJ's h79 reel opened as a video: the ∿ waveform toggle, merge
// (blade two splits, merge the first pair back, ⌘Z), delete ⌫ with an undo toast and no
// confirm, reorder with live slide (the others make room while the part is dragged), and
// trims snapping to energy onsets (≥ 12 dB rise within 40 ms).
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

async function geom(page: Page) {
  const tl = page.locator(".timeline-canvas");
  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const scroll = Number(await tl.getAttribute("data-scroll"));
  const partsY = Number((await tl.getAttribute("data-lanes-y"))!.split(",")[1]);
  const ruler = (await page.getByTestId("timeline-ruler").boundingBox())!;
  return { tl, box, ruler, ppf, x: (f: number) => box.x + 12 + f * ppf - scroll, y: box.y + partsY + 30 };
}

const partRows = (page: Page) => page.getByRole("list", { name: "Parts" }).getByRole("button");
const frames = async (page: Page) =>
  (await partRows(page).allTextContents()).map((t) => /frames (\d+)–(\d+)/.exec(t)!.slice(1).map(Number) as [number, number]);

test("waveform, merge, delete with undo, live slide, onset snapping", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-tlgaps-")), "tlgaps-test.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("dialog", (d) => { errors.push(`a dialog: ${d.message()}`); void d.dismiss(); });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("tlgaps-test", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  await expect.poll(async () => Number(await tl.getAttribute("data-audio-points")), { timeout: 120_000 }).toBeGreaterThan(0);

  // ∿ off and on again (a remembered view choice)
  await expect(tl).toHaveAttribute("data-waveform", "1");
  await page.getByTestId("waveform-toggle").click();
  await expect(tl).toHaveAttribute("data-waveform", "0");
  await page.getByTestId("waveform-toggle").click();
  await expect(tl).toHaveAttribute("data-waveform", "1");

  // onsets were found, and dragging the cut's end onto one snaps there
  await expect.poll(async () => (await tl.getAttribute("data-onsets")) ?? "", { timeout: 30_000 }).not.toBe("");
  const total = Number(await tl.getAttribute("data-frames"));
  const onsets = (await tl.getAttribute("data-onsets"))!.split(",").map(Number).filter((f) => f > total - 150 && f < total - 15);
  expect(onsets.length, "onsets in the last 5 s").toBeGreaterThan(0);
  // the playhead near the end (a click on the ruler), then zoom in around it
  let g0 = await geom(page);
  await page.mouse.click(g0.x(total - 40), g0.ruler.y + 10);
  await expect.poll(async () => Number(await page.getByTestId("timecode").getAttribute("data-frame")), { timeout: 10_000 }).toBeGreaterThan(total - 60);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  for (let k = 0; k < 5; k++) await page.keyboard.press("=");
  g0 = await geom(page);
  await page.mouse.move(g0.box.x + 200, g0.box.y + 40);
  await page.mouse.wheel(0, 4000);
  await page.waitForTimeout(200);
  g0 = await geom(page);
  expect(g0.x(total), "the cut's end on screen").toBeLessThan(g0.box.x + g0.box.width);
  let snappedTo: number | null = null;
  for (const on of onsets.slice(-5)) {
    const g = await geom(page);
    // grabbed 2 px inside the end; released 0.25 frame past the onset (in snap range)
    await page.mouse.move(g.x(total) - 2, g.y);
    await page.mouse.down();
    await page.mouse.move(g.x(on) - 2 + g.ppf * 0.25, g.y, { steps: 8 });
    const how = await tl.getAttribute("data-snapped");
    if (how === "onset") {
      await page.mouse.up();
      snappedTo = on;
      break;
    }
    await page.keyboard.press("Escape");
    await page.mouse.up();
  }
  expect(snappedTo, "an onset the end snapped to").not.toBeNull();
  await expect.poll(async () => (await frames(page)).at(-1)![1], { timeout: 20_000 }).toBe(snappedTo!);
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await frames(page)).at(-1)![1], { timeout: 20_000 }).toBe(total);
  await page.getByRole("button", { name: "Fit" }).click();

  // blade two splits: three parts
  await page.getByRole("button", { name: "Blade tool" }).click();
  let g = await geom(page);
  await page.mouse.click(g.x(Math.round(total / 3)), g.y);
  await expect(partRows(page)).toHaveCount(2, { timeout: 20_000 });
  g = await geom(page);
  await page.mouse.click(g.x(Math.round((2 * total) / 3)), g.y);
  await expect(partRows(page)).toHaveCount(3, { timeout: 20_000 });
  await page.getByRole("button", { name: "Select tool" }).click();
  const three = await frames(page);

  // merge part 1 with part 2 (they play on in the source), then ⌘Z
  g = await geom(page);
  await page.mouse.click(g.x(three[2][0] + 20), g.y);
  await expect(page.getByTestId("part-merge"), "the last part has nothing to merge with").toBeDisabled();
  await page.mouse.click(g.x(three[0][0] + 20), g.y);
  await expect(page.getByTestId("part-merge")).toBeEnabled();
  await page.getByTestId("part-merge").click();
  await expect(partRows(page)).toHaveCount(2, { timeout: 20_000 });
  expect((await frames(page))[0]).toEqual([three[0][0], three[1][1]]);
  await page.keyboard.press("ControlOrMeta+z");
  await expect(partRows(page)).toHaveCount(3, { timeout: 20_000 });

  // ⌫ deletes the selected part at once; the toast's Undo brings it back
  g = await geom(page);
  await page.mouse.click(g.x(three[1][0] + 20), g.y);
  await page.keyboard.press("Backspace");
  await expect(partRows(page)).toHaveCount(2, { timeout: 20_000 });
  await expect(page.locator(".toast")).toContainText("deleted");
  await page.getByTestId("toast-undo").click();
  await expect(partRows(page)).toHaveCount(3, { timeout: 20_000 });

  // live slide: drag part 3 to the front; mid-drag the drop slot is 0 and the others
  // have slid right (part 1 drawn after part 3's length), then the order changes
  g = await geom(page);
  const len3 = three[2][1] - three[2][0];
  await page.mouse.move(g.x(three[2][0] + 30), g.y);
  await page.mouse.down();
  await page.mouse.move(g.x(three[2][0] - 60), g.y, { steps: 5 });
  await page.mouse.move(g.x(10), g.y, { steps: 10 });
  await expect(tl).toHaveAttribute("data-drop", "0");
  await page.waitForTimeout(400);
  await page.mouse.up();
  await expect.poll(async () => (await frames(page))[0], { timeout: 20_000 }).toEqual([0, len3]);
  expect(errors).toEqual([]);
});
