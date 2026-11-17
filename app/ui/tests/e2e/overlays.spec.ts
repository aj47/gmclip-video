// Images and callouts: paste a PNG while a cut is open (it goes on the frame at the
// playhead), add a box and an arrow, place them by numbers, remove one.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("paste an image, add callouts, place and remove", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Inspector");

  // Paste a 120×60 PNG (a red square with a transparent half).
  await page.evaluate(async () => {
    const c = new OffscreenCanvas(120, 60);
    const g = c.getContext("2d")!;
    g.fillStyle = "#e5484d";
    g.fillRect(0, 0, 60, 60);
    const blob = await c.convertToBlob({ type: "image/png" });
    const dt = new DataTransfer();
    dt.items.add(new File([blob], "logo.png", { type: "image/png" }));
    document.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  const rows = page.getByTestId("overlay-row");
  await expect(rows.filter({ hasText: "Image" })).toHaveCount(1, { timeout: 30_000 });

  // A box and an arrow at the playhead.
  await page.getByTestId("callout-box").click();
  await expect(page.locator('[data-testid="overlay-row"][data-kind="box"]')).toHaveCount(1, { timeout: 20_000 });
  await page.getByTestId("callout-arrow").click();
  await expect(page.locator('[data-testid="overlay-row"][data-kind="arrow"]')).toHaveCount(1, { timeout: 20_000 });

  // Place the box by numbers: its width becomes 50 %.
  const box = page.locator('[data-testid="overlay-row"][data-kind="box"]');
  await box.getByRole("group", { name: "Box" }).getByLabel("w").fill("50");
  await box.getByRole("group", { name: "Box" }).getByLabel("w").press("Enter");
  // (data-value / data-on are the cut as read back, not what the field shows: ⌘Z below
  // must undo the removal, so every edit before it has to have landed)
  await expect(box.getByRole("group", { name: "Box" }).getByLabel("w")).toHaveAttribute("data-value", "50", { timeout: 20_000 });

  // The blurred-source fill behind the layers (and back to solid).
  await page.getByTestId("blurred-fill").check();
  await expect(page.getByTestId("blurred-fill")).toHaveAttribute("data-on", "1", { timeout: 20_000 });

  if (process.env.GMCLIP_SHOT) {
    await page.waitForTimeout(1500);
    await page.getByRole("region", { name: "Viewer" }).screenshot({ path: process.env.GMCLIP_SHOT });
  }

  // Remove the arrow; undo brings it back.
  await page.locator('[data-testid="overlay-row"][data-kind="arrow"]').getByRole("button", { name: "Remove" }).click();
  await expect(page.locator('[data-testid="overlay-row"][data-kind="arrow"]')).toHaveCount(0, { timeout: 20_000 });
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.locator('[data-testid="overlay-row"][data-kind="arrow"]')).toHaveCount(1, { timeout: 20_000 });
  await page.getByTestId("blurred-fill").uncheck();
  await expect(page.getByTestId("blurred-fill")).toHaveAttribute("data-on", "0", { timeout: 20_000 });
  expect(errors).toEqual([]);
});
