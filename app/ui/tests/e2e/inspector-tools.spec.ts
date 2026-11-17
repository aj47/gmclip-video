// The Inspector's per-part tools on one opened video (each test leaves the cut as it found
// it): speed per part (decision 0030: 2× stretches the sound with pitch kept and halves the
// part, 4× is silent, back to 1×), a speed ramp (decision 0033: 1× → 2× over 2 s from the
// playhead, 55 s → 28 s, Remove ramp puts it back), and pin to motion (decisions 0031,
// 0033; Apple Vision, macOS): a box callout and a blur cover each follow what is under them,
// say so, and unpin.
import { expect, test, type Browser } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

async function open(browser: Browser) {
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
  await openTab(page, "Inspector");
  return { page, errors };
}

test("a part at 2×, then 4× (silent), then back to 1×", async ({ browser }) => {
  test.setTimeout(240_000);
  const { page, errors } = await open(browser);
  const total = page.locator(".tc-total");
  await expect(total).toContainText("55", { timeout: 20_000 });

  const speed = page.getByTestId("part-speed");
  await speed.selectOption("2");
  await expect(speed).toHaveValue("2", { timeout: 20_000 });
  await expect(total).toContainText("27", { timeout: 20_000 });

  await speed.selectOption("4");
  await expect(page.locator(".toast").filter({ hasText: "silent" })).toBeVisible({ timeout: 20_000 });

  await speed.selectOption("1");
  await expect(speed).toHaveValue("1", { timeout: 20_000 });
  await expect(total).toContainText("55", { timeout: 20_000 });
  expect(errors).toEqual([]);
});

test("a ramp from 1× to 2×, then removed", async ({ browser }) => {
  test.setTimeout(240_000);
  const { page, errors } = await open(browser);
  const total = page.locator(".tc-total");
  await expect(total).toContainText("55", { timeout: 20_000 });

  await page.getByTestId("ramp-to").selectOption("2");
  await page.getByTestId("ramp-ms").fill("2000");
  await page.getByTestId("ramp-add").click();
  await expect(page.getByTestId("ramp-now")).toContainText("1× → 2×", { timeout: 20_000 });
  await expect(page.getByTestId("ramp-now")).toContainText("ease in out");
  await expect(total).toContainText("28", { timeout: 20_000 });

  await page.getByTestId("ramp-remove").click();
  await expect(page.getByTestId("ramp-add")).toBeVisible({ timeout: 20_000 });
  await expect(total).toContainText("55", { timeout: 20_000 });
  expect(errors).toEqual([]);
});

test("pin a callout to motion, then unpin it", async ({ browser }) => {
  test.skip(process.platform !== "darwin", "motion tracking uses Apple Vision");
  test.setTimeout(240_000);
  const { page, errors } = await open(browser);

  await page.getByTestId("callout-box").click();
  const box = page.locator('[data-testid="overlay-row"][data-kind="box"]');
  await expect(box).toHaveCount(1, { timeout: 20_000 });
  await box.getByTestId("track-pin").click();
  await expect(box.getByTestId("track-pinned")).toBeVisible({ timeout: 60_000 });
  await expect(page.locator(".toast").filter({ hasText: "Pinned" })).toBeVisible({ timeout: 20_000 });

  await box.getByTestId("track-unpin").click();
  await expect(box.getByTestId("track-pin")).toBeVisible({ timeout: 20_000 });
  expect(errors).toEqual([]);
});

test("blur a region, pin the cover to motion, then unpin it", async ({ browser }) => {
  test.skip(process.platform !== "darwin", "motion tracking uses Apple Vision");
  test.setTimeout(300_000);
  const { page, errors } = await open(browser);

  await page.getByTestId("cover-add").click();
  const row = page.getByTestId("cover-row");
  await expect(row).toHaveCount(1, { timeout: 20_000 });
  await expect(row).toContainText("Blur");
  await row.getByTestId("cover-pin").click();
  await expect(row.getByTestId("cover-pinned")).toBeVisible({ timeout: 240_000 });
  await expect(page.locator(".toast").filter({ hasText: "pinned" })).toBeVisible({ timeout: 20_000 });

  await row.getByTestId("cover-unpin").click();
  await expect(row.getByTestId("cover-pin")).toBeVisible({ timeout: 20_000 });
  expect(errors).toEqual([]);
});
