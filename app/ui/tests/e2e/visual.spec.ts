// U.17 visual regression (SPEC §17 UI): five screens at 1100 × 700 (R-UI-1's smaller size)
// compared with committed baselines — Home on a fresh library, the cut (viewer, words,
// transcript, timeline), the keys help, the palette and the Short tab. What moves by
// itself is masked: the decoded video frame, the
// timeline's canvas (waveform tiles stream in), the render stats and the timecode.
// Baselines: `visual.spec.ts-snapshots/`, refreshed with `--update-snapshots` by a change
// that means to move pixels (the commit says why). Playwright's own comparison; no new
// dependency.
import { expect, test, type Locator, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openPage, VIDEO } from "./ux-lib";
import { hasLab } from "./lab";
import { openTab } from "./tabs";
import { transcribed } from "./analysed";


test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
// the baselines are the Mac's (fonts, menus and key labels differ by platform)
test.skip(process.platform !== "darwin", "visual baselines are macOS only");
test.skip(!existsSync(VIDEO) || !hasLab("preview"), "no test video or lab media on this machine");

const masks = (page: Page): Locator[] => [
  page.locator(".viewer-canvas"),
  page.locator(".timeline-canvas"),
  page.getByTestId("viewer-stats"),
  page.getByTestId("timecode"),
  page.locator(".toast"),
];

async function same(page: Page, name: string) {
  // fonts in, no caret, transitions settled
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(400);
  await expect(page).toHaveScreenshot(`${name}.png`, {
    mask: masks(page),
    animations: "disabled",
    caret: "hide",
    maxDiffPixelRatio: 0.01,
    timeout: 20_000,
  });
}

test("six screens look as they did", async ({ browser }) => {
  test.setTimeout(300_000);
  const { page, errors } = await openPage(browser, { width: 1100, height: 700 });
  // 1. Home on a fresh library
  await expect(page.getByLabel("Video path")).toBeVisible();
  await same(page, "home");

  // 2. the cut: h79 opened, its words heard
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-vis-")), "visual-test.mp4");
  copyFileSync(VIDEO, copy);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("visual-test", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  await expect(page.getByRole("listbox", { name: "Cut words" })).toBeVisible({ timeout: 30_000 });
  // the first-run tip goes at the first click (it is shown once per Mac)
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await expect(page.getByTestId("coach-welcome")).toHaveCount(0);
  await same(page, "cut");

  // 3. the keys help
  await page.keyboard.press("?");
  const help = page.getByRole("dialog", { name: /Keyboard/ });
  await expect(help).toBeVisible();
  await same(page, "keys");
  await help.getByRole("button", { name: "Close" }).click();

  // 4. the palette, searching
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("export");
  await same(page, "palette");
  await page.getByTestId("palette-input").press("Escape");

  // 5. the Short tab
  await openTab(page, "Short");
  await expect(page.getByLabel("Hook text")).toBeVisible();
  await same(page, "short");

  expect(errors).toEqual([]);
  await page.close();
});
