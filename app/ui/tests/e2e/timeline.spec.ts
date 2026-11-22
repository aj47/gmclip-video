// U.08 remainder on h79 (imported RenderPlan): overlay lanes are greedy-packed (no two
// overlays of a lane overlap, as many lanes as the most that overlap); ⌘-click marks,
// ⇧-click makes a range from the mark; ⌥L loops it (still playing, inside the range,
// after the range's length has passed); hover over the ruler scrubs. An imported plan's
// text and envelopes are read-only lanes: the gain lane lists h79's two envelopes and a
// click or drag there writes nothing.
import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labPlan } from "./lab";

const H79 = labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json"));

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

const frameOf = async (page: Page) => Number(/(\d+)/.exec((await page.getByTestId("timecode").getAttribute("data-frame")) ?? "")?.[1] ?? NaN);

test("overlay lanes, marks, range loop, hover scrub", async ({ browser }) => {
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await importPlanProject(page, `Lanes ${Date.now()}`, H79);
  await expect(page.getByTestId("cut-title")).toHaveText("My PC made this in 67 seconds", { timeout: 60_000 });

  // Overlay lanes: greedy packing.
  const items = page.getByRole("list", { name: "Overlays" }).getByRole("button");
  await expect.poll(() => items.count()).toBeGreaterThanOrEqual(2);
  const rows = (await items.allTextContents()).map((t) => {
    const m = /frames (\d+)–(\d+), lane (\d+)/.exec(t)!;
    return { a: Number(m[1]), b: Number(m[2]), lane: Number(m[3]) };
  });
  for (const x of rows) for (const y of rows) if (x !== y && x.lane === y.lane) expect(x.b <= y.a || y.b <= x.a, `${JSON.stringify(x)} ${JSON.stringify(y)}`).toBe(true);
  const most = Math.max(...rows.map((x) => rows.filter((y) => y.a < x.b && x.a < y.b && y.a <= x.a).length));
  const tl = page.locator(".timeline-canvas");
  await expect(tl).toHaveAttribute("data-overlay-lanes", String(Math.max(...rows.map((r) => r.lane))));
  expect(Math.max(...rows.map((r) => r.lane))).toBe(most);

  // The plan's envelopes in the gain lane, read-only.
  await expect(tl).toHaveAttribute("data-overlays-editable", "0");
  const gainRows = page.getByRole("list", { name: "Volume" }).getByRole("button");
  await expect(gainRows).toHaveCount(2, { timeout: 20_000 });
  const before = await gainRows.allTextContents();
  expect(before[0]).toContain("+10 dB (frames 1–54)");
  {
    const b = (await tl.boundingBox())!;
    const p = Number(await tl.getAttribute("data-ppf"));
    const gy = b.y + Number((await tl.getAttribute("data-lanes-y"))!.split(",")[2]) + 6;
    await page.mouse.click(b.x + 12 + 600 * p, gy);
    await page.mouse.move(b.x + 12 + 20 * p, gy);
    await page.mouse.down();
    await page.mouse.move(b.x + 12 + 200 * p, gy, { steps: 4 });
    await page.mouse.up();
    await page.waitForTimeout(500);
    expect(await gainRows.allTextContents()).toEqual(before);
  }

  // Marks and a range, on the ruler (on a part, ⌘- and ⇧-click select parts, decision 0037).
  const box = (await tl.boundingBox())!;
  const rul = (await page.getByTestId("timeline-ruler").boundingBox())!; // the pinned ruler
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const xAt = (f: number) => box.x + 12 + f * ppf;
  await page.keyboard.down("ControlOrMeta");
  await page.mouse.click(xAt(300), rul.y + 10);
  await page.keyboard.up("ControlOrMeta");
  await expect(tl).toHaveAttribute("data-marks", /^\d+$/);
  const mark = Number(await tl.getAttribute("data-marks"));
  expect(Math.abs(mark - 300)).toBeLessThanOrEqual(2);
  await page.keyboard.down("Shift");
  await page.mouse.click(xAt(360), rul.y + 10);
  await page.keyboard.up("Shift");
  await expect(tl).toHaveAttribute("data-range", new RegExp(`^${mark}-\\d+$`));
  const [ra, rb] = (await tl.getAttribute("data-range"))!.split("-").map(Number) as [number, number];

  // ⌥L loops it: after more than the range's length it still plays, inside the range.
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("Alt+KeyL");
  await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeVisible({ timeout: 5_000 });
  await page.waitForTimeout(((rb - ra) / 30) * 1000 + 1500);
  await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeVisible();
  const f = await frameOf(page);
  expect(f >= ra - 1 && f <= rb + 1, `frame ${f} in ${ra}–${rb}`).toBe(true);
  await page.keyboard.press("k");
  await expect(page.getByRole("button", { name: "Play", exact: true })).toBeVisible();

  // Hover scrub on the ruler.
  await page.mouse.move(xAt(900), rul.y + 6);
  await page.mouse.move(xAt(1000), rul.y + 6, { steps: 3 });
  await expect.poll(() => frameOf(page), { timeout: 5_000 }).toBeGreaterThan(950);

  await page.getByTestId("marks-clear").click();
  await expect(tl).toHaveAttribute("data-marks", "");
  expect(errors).toEqual([]);
});
