// Decision 0053: a part's waveform stays at the timeline's scale on its own frames while
// its edge is dragged (AJ's h79 reel as a video, bladed in two). Shortening part 1 cuts
// the waveform off at the edge and lengthening it reveals its source past the edge; the
// drawing over the rest of the part does not move (before: it was stretched or squashed
// to the part's new width).
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
  return { box, ppf, partsY, cx: (f: number) => 12 + f * ppf - scroll, x: (f: number) => box.x + 12 + f * ppf - scroll, y: box.y + partsY + 30 };
}

/** The canvas's pixels over css x `[x0, x1)` in the lower part of the parts lane (the
 * waveform band), as a string, and how many distinct pixels it has. */
const band = (page: Page, x0: number, x1: number, partsY: number) =>
  page.evaluate(
    ([x0, x1, py]) => {
      const c = document.querySelector<HTMLCanvasElement>(".timeline-canvas")!;
      const k = c.width / c.clientWidth;
      const d = c.getContext("2d")!.getImageData(Math.round(x0 * k), Math.round((py + 46) * k), Math.round((x1 - x0) * k), Math.round(20 * k)).data;
      const px = new Set<number>();
      let s = "";
      for (let i = 0; i < d.length; i += 4) {
        const v = (d[i]! << 16) | (d[i + 1]! << 8) | d[i + 2]!;
        px.add(v);
        s += v.toString(36) + ",";
      }
      return { s, distinct: px.size };
    },
    [x0, x1, partsY] as [number, number, number],
  );

const partRows = (page: Page) => page.getByRole("list", { name: "Parts" }).getByRole("button");

test("a trim drag cuts off or reveals the waveform, never stretches it", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-trimwave-")), "trim-wave.mp4");
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
  await expect(page.getByTestId("cut-title")).toHaveText("trim-wave", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  await expect.poll(async () => Number(await tl.getAttribute("data-audio-points")), { timeout: 120_000 }).toBeGreaterThan(0);
  await expect(tl).toHaveAttribute("data-waveform", "1");
  const total = Number(await tl.getAttribute("data-frames"));

  await page.getByRole("button", { name: "Blade tool" }).click();
  let g = await geom(page);
  await page.mouse.click(g.x(Math.round(total / 2)), g.y);
  await expect(partRows(page)).toHaveCount(2, { timeout: 20_000 });
  await page.getByRole("button", { name: "Select tool" }).click();
  const end = Number(/frames \d+–(\d+)/.exec((await partRows(page).first().textContent())!)![1]);
  g = await geom(page);
  // part 1 selected, the pointer resting on its out edge (as during the drag)
  await page.mouse.click(g.x(end * 0.2), g.y);
  await page.mouse.move(g.x(end) - 2, g.y);
  await page.waitForTimeout(300);
  const mid: [number, number] = [g.cx(end * 0.3), g.cx(end * 0.6)];
  const still = await band(page, ...mid, g.partsY);
  expect(still.distinct, "a waveform is drawn there").toBeGreaterThan(3);

  await page.mouse.down();
  await page.keyboard.down("Alt");
  // shorter by 80 frames: the rest of the part is drawn as it was
  await page.mouse.move(g.x(end - 80) - 2, g.y, { steps: 6 });
  await expect(page.getByTestId("trim-readout")).toHaveText("−80f");
  await page.waitForTimeout(150);
  expect((await band(page, ...mid, g.partsY)).s === still.s, "shortening leaves the waveform where it was").toBe(true);

  // longer by 80: the same, and the source past the old edge is drawn in the new room
  await page.mouse.move(g.x(end + 80) - 2, g.y, { steps: 10 });
  await expect(page.getByTestId("trim-readout")).toHaveText("+80f");
  await expect.poll(async () => Number(await tl.getAttribute("data-edge-levels")), { timeout: 10_000 }).toBeGreaterThan(0);
  await page.waitForTimeout(150);
  expect((await band(page, ...mid, g.partsY)).s === still.s, "lengthening leaves the waveform where it was").toBe(true);
  expect((await band(page, g.cx(end + 10), g.cx(end + 70), g.partsY)).distinct, "the revealed source is drawn").toBeGreaterThan(3);

  await page.keyboard.press("Escape");
  await page.mouse.up();
  await page.keyboard.up("Alt");
  expect(errors).toEqual([]);
});
