// Part groups (decision 0037) on AJ's h79 reel opened as a video: blade it into four
// parts of different lengths, ⌘-click the first and the third, ⌘G groups them; dragging
// one moves both, landing back to back at the drop; ⌘Z puts them back; a click on a
// grouped part selects the group, and ⌘⇧G ungroups it.
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
  return { box, x: (f: number) => box.x + 12 + f * ppf - scroll, y: box.y + partsY + 30 };
}

const partRows = (page: Page) => page.getByRole("list", { name: "Parts" }).getByRole("button");
const frames = async (page: Page) =>
  (await partRows(page).allTextContents()).map((t) => /frames (\d+)–(\d+)/.exec(t)!.slice(1).map(Number) as [number, number]);
/** Each part's length in frames, in the cut's order. */
const lengths = async (page: Page) => (await frames(page)).map(([a, b]) => b - a);

test("group parts, drag them together, ungroup", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-groups-")), "groups-test.mp4");
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
  await expect(page.getByTestId("cut-title")).toHaveText("groups-test", { timeout: 30_000 });
  const tl = page.locator(".timeline-canvas");
  const total = Number(await tl.getAttribute("data-frames"));

  // blade at 10 %, 30 % and 60 %: parts A B C D of 10, 20, 30 and 40 %
  await page.getByRole("button", { name: "Blade tool" }).click();
  for (const [k, at] of [0.1, 0.3, 0.6].entries()) {
    const g = await geom(page);
    await page.mouse.click(g.x(Math.round(total * at)), g.y);
    await expect(partRows(page)).toHaveCount(k + 2, { timeout: 20_000 });
  }
  await page.getByRole("button", { name: "Select tool" }).click();
  const [la, lb, lc, ld] = await lengths(page);
  expect(new Set([la, lb, lc, ld]).size, "four lengths tell the parts apart").toBe(4);

  // ⌘-click A and C: Group is offered; ⌘G groups them
  const group = page.getByTestId("parts-group");
  const ungroup = page.getByTestId("parts-ungroup");
  let f = await frames(page);
  let g = await geom(page);
  await page.mouse.click(g.x(f[0]![0] + 10), g.y);
  await expect(group).toBeDisabled();
  await page.keyboard.down("ControlOrMeta");
  await page.mouse.click(g.x(f[2]![0] + 10), g.y);
  await page.keyboard.up("ControlOrMeta");
  await expect(group).toBeEnabled();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+g");
  await expect(page.locator(".toast")).toContainText("2 parts grouped");
  await expect(ungroup).toBeEnabled();

  // drag C past the end: A and C land back to back after B and D
  f = await frames(page);
  g = await geom(page);
  await page.mouse.move(g.x(f[2]![0] + 10), g.y);
  await page.mouse.down();
  await page.mouse.move(g.x(total) + 4, g.y, { steps: 12 });
  await page.mouse.up();
  await expect.poll(() => lengths(page), { timeout: 20_000 }).toEqual([lb, ld, la, lc]);

  // ⌘Z: one step back to A B C D
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => lengths(page), { timeout: 20_000 }).toEqual([la, lb, lc, ld]);

  // a click on B: not grouped; a click on C selects its group; ⌘⇧G ungroups
  f = await frames(page);
  g = await geom(page);
  await page.mouse.click(g.x(f[1]![0] + 10), g.y);
  await expect(ungroup).toBeDisabled();
  await page.mouse.click(g.x(f[2]![0] + 10), g.y);
  await expect(ungroup).toBeEnabled();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+Shift+g");
  await expect(page.locator(".toast")).toContainText("Ungrouped");
  await expect(ungroup).toBeDisabled();

  // ungrouped: dragging A moves A alone
  f = await frames(page);
  g = await geom(page);
  await page.mouse.move(g.x(f[0]![0] + 10), g.y);
  await page.mouse.down();
  await page.mouse.move(g.x(total) + 4, g.y, { steps: 12 });
  await page.mouse.up();
  await expect.poll(() => lengths(page), { timeout: 20_000 }).toEqual([lb, lc, ld, la]);

  // a band: a drag in the empty space under the lanes selects the parts it spans (B, C)
  f = await frames(page);
  g = await geom(page);
  const tlBox = (await tl.boundingBox())!;
  const ovY = Number((await tl.getAttribute("data-lanes-y"))!.split(",")[4]);
  const under = Math.min(tlBox.y + tlBox.height - 4, tlBox.y + ovY + 30);
  await page.mouse.click(g.x(f[3]![0] + 10), g.y);
  await expect(group).toBeDisabled();
  await page.mouse.move(g.x(f[0]![0] + 10), under);
  await page.mouse.down();
  await page.mouse.move(g.x(f[1]![0] + 10), under, { steps: 8 });
  await page.mouse.up();
  await expect(group).toBeEnabled();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+g");
  await expect(page.locator(".toast")).toContainText("2 parts grouped");
  // ⇧-drag across the parts themselves selects the run it crosses (C, D, A: three)
  g = await geom(page);
  await page.keyboard.down("Shift");
  await page.mouse.move(g.x(f[1]![0] + 10), g.y);
  await page.mouse.down();
  await page.mouse.move(g.x(f[3]![0] + 10), g.y, { steps: 8 });
  await page.mouse.up();
  await page.keyboard.up("Shift");
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+g");
  await expect(page.locator(".toast")).toContainText("4 parts grouped");
  // a click in the empty space clears the selection, and no gain segment was made
  await page.mouse.click(g.x(f[0]![0] + 10), under);
  await expect(page.getByRole("list", { name: "Gain" }).getByRole("button")).toHaveCount(0);
  expect(errors).toEqual([]);
});
