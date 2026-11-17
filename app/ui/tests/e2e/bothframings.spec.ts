// 9:16 and 16:9 together (lane K5) on AJ's h79 reel (a 9:16 video, so its cut has only the
// vertical framing): the export popover offers to make the 16:9 from the layout defaults,
// then exports both as two jobs with their own records; the cut keeps showing its 9:16;
// "Several cuts…" opens the sheet with this cut ticked and both framings exported; Home's
// project card opens the same sheet. Fits a 480 × 360 window.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bridge, open, projectNamed } from "./qa-lib";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");
const SHOTS = process.env.GMCLIP_SHOTS_DIR;

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("both framings from the export popover and the sheet", async ({ browser }) => {
  test.setTimeout(300_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-both-")), "both-test.mp4");
  copyFileSync(VIDEO, copy);
  const { page, errors } = await open(browser);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("both-test", { timeout: 30_000 });
  const pid = (await projectNamed(page, "both-test"))!.id;
  const output = page.getByRole("group", { name: "Output" });
  await expect(output.getByRole("button", { name: "9:16" })).toHaveAttribute("aria-pressed", "true");

  // the popover: 9:16 not exported, 16:9 to be made; asking first
  await page.getByRole("button", { name: "Export options" }).click();
  const box = page.getByTestId("export-both-box");
  await expect(box.getByTestId("framing-state")).toContainText("9:16 · not exported");
  await expect(box.getByTestId("framing-missing")).toContainText("16:9");
  await box.getByTestId("export-both").click();
  await expect(box.getByTestId("export-both-ask")).toContainText("no 16:9 yet");
  if (SHOTS) await page.screenshot({ path: join(SHOTS, "export-both-ask-1440x900.png") });
  await box.getByTestId("export-both-make").click();
  // two exports, each with its own record and framing
  await expect.poll(async () => (await bridge<unknown[]>(page, "exports_list", {}, pid)).length, { timeout: 240_000, intervals: [1000] }).toBe(2);
  const ex = await bridge<{ file: string }[]>(page, "exports_list", {}, pid);
  expect(ex.map((e) => e.file).some((f) => f.includes("9x16"))).toBe(true);
  expect(ex.map((e) => e.file).some((f) => f.includes("16x9"))).toBe(true);
  // the cut still shows its 9:16
  await expect(output.getByRole("button", { name: "9:16" })).toHaveAttribute("aria-pressed", "true");
  if (!(await box.isVisible())) await page.getByRole("button", { name: "Export options" }).click();
  await expect(box.getByTestId("framing-state")).toHaveCount(2);
  await expect(box.getByTestId("framing-state").nth(0)).toContainText("9:16 · exported");
  await expect(box.getByTestId("framing-state").nth(1)).toContainText("16:9 · exported");

  // Several cuts…: the sheet, this cut ticked, both exported
  await box.getByTestId("framings-open").click();
  const sheet = page.getByTestId("framings-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet.getByTestId("framings-cut").first()).toBeChecked();
  await expect(sheet.getByTestId("framings-row").first().getByTestId("framing-state")).toHaveCount(2);
  await expect(sheet.getByTestId("framings-start")).toContainText("Export 1 cut · 2 files");
  // a small window: the sheet still fits and scrolls inside itself
  await page.setViewportSize({ width: 480, height: 360 });
  const b = (await sheet.boundingBox())!;
  expect(b.x).toBeGreaterThanOrEqual(0);
  expect(b.x + b.width).toBeLessThanOrEqual(480);
  expect(b.y + b.height).toBeLessThanOrEqual(360);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
  expect(overflow).toBe(false);
  if (SHOTS) await page.screenshot({ path: join(SHOTS, "framings-sheet-480x360.png") });
  await page.setViewportSize({ width: 1440, height: 900 });
  await sheet.getByRole("button", { name: "Close" }).click();
  await expect(sheet).toHaveCount(0);

  // Home's card opens it for the project
  await page.getByRole("button", { name: "Home" }).click();
  await page.locator(".project").filter({ hasText: "both-test" }).first().getByTestId("framings-open-home").click();
  await expect(page.getByTestId("framings-sheet")).toBeVisible();
  await expect(page.getByTestId("framings-sheet").getByTestId("framings-start")).toBeDisabled();
  expect(errors).toEqual([]);
});
