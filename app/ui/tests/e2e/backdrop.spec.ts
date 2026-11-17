// Backdrops and video clips (decision 0028): a video under every part (stored once, looping
// on across the cuts), its fit, a video picture-in-picture at the playhead, and back to the
// plain background.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("a video backdrop, its fit, a video clip on top", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toContainText("why-i-give", { timeout: 30_000 });
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Inspector");

  // The recording itself as the backdrop: stored once, looping, filling the frame.
  await page.getByTestId("media-path").fill(VIDEO);
  await page.getByTestId("backdrop-set").click();
  await expect(page.getByTestId("backdrop-on")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("backdrop-name")).toContainText("own-vm");
  await expect(page.getByTestId("backdrop-loop")).toBeChecked();
  await page.getByTestId("backdrop-fit").selectOption("contain");
  await expect(page.getByTestId("backdrop-fit")).toHaveValue("contain", { timeout: 20_000 });
  await page.getByTestId("backdrop-loop").uncheck();
  await expect(page.getByTestId("backdrop-loop")).not.toBeChecked({ timeout: 20_000 });

  // A video clip on top at the playhead, then placed by numbers.
  await page.getByTestId("media-path").fill(VIDEO);
  await page.getByTestId("video-overlay-add").click();
  const clip = page.locator('[data-testid="overlay-row"][data-kind="video"]');
  await expect(clip).toHaveCount(1, { timeout: 30_000 });
  await clip.getByRole("group", { name: "Place" }).getByLabel("w").fill("30");
  await clip.getByRole("group", { name: "Place" }).getByLabel("w").press("Enter");
  await expect(clip.getByRole("group", { name: "Place" }).getByLabel("w")).toHaveValue("30", { timeout: 20_000 });

  if (process.env.GMCLIP_SHOT) {
    await page.waitForTimeout(1500);
    await page.getByRole("region", { name: "Viewer" }).screenshot({ path: process.env.GMCLIP_SHOT });
  }

  // Back to the plain background; undo brings the backdrop back.
  await page.getByRole("button", { name: "Remove backdrop" }).click();
  await expect(page.getByTestId("backdrop-on")).toHaveCount(0, { timeout: 20_000 });
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.getByTestId("backdrop-on")).toBeVisible({ timeout: 20_000 });
  expect(errors).toEqual([]);
});

test("background plates: a built-in one, then my own", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toContainText("why-i-give", { timeout: 30_000 });
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Inspector");

  // A built-in plate (decision 0029).
  await page.getByTestId("plate-pick").selectOption("Stream blue");
  await expect(page.getByTestId("plate-pick")).toHaveValue("Stream blue", { timeout: 20_000 });
  await expect(page.getByTestId("blurred-fill")).not.toBeChecked();

  // My own: two colours and a name, saved and used.
  await page.getByTestId("plate-top").fill("#101820");
  await page.getByTestId("plate-bottom").fill("#fee715");
  await page.getByTestId("plate-name").fill("Brand");
  await page.getByTestId("plate-save").click();
  await expect(page.getByTestId("plate-pick")).toHaveValue("Brand", { timeout: 20_000 });

  if (process.env.GMCLIP_SHOT) {
    await page.waitForTimeout(1500);
    await page.getByRole("region", { name: "Viewer" }).screenshot({ path: process.env.GMCLIP_SHOT });
  }

  // None: back to the plain colour.
  await page.getByTestId("plate-pick").selectOption("");
  await expect(page.getByTestId("plate-pick")).toHaveValue("", { timeout: 20_000 });
  expect(errors).toEqual([]);
});
