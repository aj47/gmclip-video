// AJ 2026-10-20: the layer selected on the preview is marked on its timeline row; a click
// on a row's name groups every clip of the row, and a crop set on one grouped layer is set
// on all of them (one undo); a plain click outside the group ends it.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("group a layer across parts on the timeline and crop them together", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-booted", "1", { timeout: 60_000 });
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Layout");
  await page.getByRole("button", { name: "Screen + facecam" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");

  // two parts, both Screen + facecam
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:01\.20/);
  await page.keyboard.press("s");
  await expect.poll(async () => (await page.getByTestId("cut-meta").textContent()) ?? "").toContain("2 parts");

  const camRow = page.locator('[data-testid=layer-track][data-key="layer:cam"]');
  const camClips = camRow.getByTestId("layer-track-layer-clip");
  await expect(camClips).toHaveCount(2);

  // a click on a clip selects that layer: its clip and its row are marked, not the others
  await camClips.nth(0).click();
  await expect(camClips.nth(0)).toHaveAttribute("aria-pressed", "true", { timeout: 10_000 });
  await expect(camRow).toHaveClass(/\bsel\b/);
  await expect(page.locator('[data-testid=layer-track][data-key="layer:screen"]')).not.toHaveClass(/\bsel\b/);

  // the row's name groups both camera clips
  await camRow.getByTestId("layer-track-name").click();
  await expect(camClips.nth(0)).toHaveAttribute("data-grouped", "true");
  await expect(camClips.nth(1)).toHaveAttribute("data-grouped", "true");
  await openTab(page, "Inspector");
  const insp = page.getByRole("complementary", { name: "Inspector" });
  await expect(insp.getByTestId("insp-group")).toContainText("grouped on 2 parts");

  // a crop typed on part 1's camera is part 2's too
  const camX = () => insp.getByRole("region", { name: "Layer cam" }).getByRole("group", { name: "Crop (source pixels)" }).getByLabel("x");
  await insp.getByRole("region", { name: "Layer cam" }).locator("summary").click();
  const x0 = await camX().inputValue();
  const want = String(Number(x0) > 300 ? 120 : 360);
  await camX().fill(want);
  await camX().press("Enter");
  await expect(camX()).toHaveValue(want, { timeout: 20_000 });
  await camClips.nth(1).click();
  await expect(camClips.nth(1)).toHaveAttribute("aria-pressed", "true", { timeout: 10_000 });
  await expect(camClips.nth(0)).toHaveAttribute("data-grouped", "true");
  await expect(camX()).toHaveValue(want, { timeout: 20_000 });

  // one undo puts both back
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(camX()).toHaveValue(x0, { timeout: 20_000 });

  // a plain click outside the group ends it
  await page.locator('[data-testid=layer-track][data-key="layer:screen"]').getByTestId("layer-track-layer-clip").nth(0).click();
  await expect(camClips.nth(0)).toHaveAttribute("data-grouped", "false");
  await expect(insp.getByTestId("insp-group")).toHaveCount(0);
  expect(errors).toEqual([]);
});
