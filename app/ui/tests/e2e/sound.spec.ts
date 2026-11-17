// Reel-tools parity in the Inspector: gain over a span and the camera hidden over a span
// (marked frame → playhead), listed with ✕ to remove; part gain.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("gain and camera spans", async ({ browser }) => {
  test.setTimeout(120_000);
  // its own copy: other specs edit h79's project in the same library
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-sound-")), "sound-test.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("sound-test", { timeout: 30_000 });
  await openTab(page, "Inspector");
  // the inspector's sound is in the Look tab, under the layout (decision 0054), with the
  // music row (decision 0051)
  const box = page.getByTestId("look-pane").getByRole("region", { name: "Sound and camera" });
  await expect(box).toBeVisible();
  await expect(box.getByRole("group", { name: "Music" })).toBeVisible();
  await page.locator(".cut-head").hover();
  const tc = page.getByTestId("timecode");
  await expect(tc).toBeVisible({ timeout: 30_000 });
  await page.keyboard.press("Home");
  await expect(tc).toHaveText(/^0:00\.00 \//);
  for (let i = 0; i < 30; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(tc).toHaveText(/^0:10\.00 \//);
  await box.getByTestId("mark-span").click();
  await page.locator(".cut-head").hover();
  for (let i = 0; i < 15; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(box).toContainText("10.00–15.00 s (to the playhead)", { timeout: 10_000 });
  await box.getByTestId("gain-span").click();
  await expect(box.getByTestId("gain-row")).toContainText("+10 dB · 10.00–15.00 s", { timeout: 10_000 });
  // and the camera over another span
  await box.getByTestId("mark-span").click();
  await page.locator(".cut-head").hover();
  for (let i = 0; i < 10; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(box).toContainText("15.00–18.33 s (to the playhead)");
  await box.getByTestId("hide-cam").click();
  await expect(box.getByTestId("hide-row")).toContainText("Camera hidden · 15.00–18.33 s", { timeout: 10_000 });
  await box.getByRole("button", { name: "Remove this gain" }).click();
  await expect(box.getByTestId("gain-row")).toHaveCount(0, { timeout: 10_000 });
  await box.getByRole("button", { name: "Show the camera again" }).click();
  await expect(box.getByTestId("hide-row")).toHaveCount(0, { timeout: 10_000 });
  // one step per take: the camera hidden and no captions on the take under the playhead
  await box.getByTestId("take-hide-cam").check();
  await expect(box.getByTestId("hide-row")).toHaveCount(1, { timeout: 10_000 });
  await box.getByTestId("take-no-captions").check();
  await expect(box.getByTestId("quiet-row")).toHaveCount(1, { timeout: 10_000 });
  await box.getByTestId("take-hide-cam").uncheck();
  await expect(box.getByTestId("hide-row")).toHaveCount(0, { timeout: 10_000 });
  await box.getByTestId("take-no-captions").uncheck();
  await expect(box.getByTestId("quiet-row")).toHaveCount(0, { timeout: 10_000 });
  expect(errors).toEqual([]);
});
