// Match a caption style from a short (P.10): AJ's h79 v4 reel (RT captions: Neometric
// Heavy 86, outline 12, yellow keywords, at y 1065 of 1920) measured from its picture;
// the result says it is an approximation, lands in the gallery as "≈ h79-v4" and applies
// as one undo step.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");
const SHORT = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO) || !existsSync(SHORT), "no test videos on this machine");

test("a caption style matched from a short", async ({ browser }) => {
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
  await page.getByTestId("gallery-toggle").click();
  const t0 = Date.now();
  await page.getByTestId("match-path").fill(SHORT);
  await page.getByTestId("match-path").press("Enter");
  const res = page.getByTestId("match-result");
  await expect(res).toBeVisible({ timeout: 120_000 });
  console.log(`matched in ${Date.now() - t0} ms: ${await res.textContent()} keyword=${await res.getAttribute("data-keyword")} outline=${await res.getAttribute("data-outline")}`);
  await expect(res).toContainText("≈ h79-v4");
  await expect(page.getByTestId("match-approx")).toHaveText("approximation");
  const text = (await res.textContent()) ?? "";
  // size near RT's 86, heavy, an outline, at ~55 % of the height, a yellow keyword
  const size = Number(/(\d+) px · (\w+)/.exec(text)![1]);
  expect(size).toBeGreaterThan(65);
  expect(size).toBeLessThan(110);
  expect(text).toMatch(/outline \d+ px/);
  expect(Number(await res.getAttribute("data-outline"))).toBeGreaterThanOrEqual(6);
  // RT's keyword yellow (#FFD400)
  const kw = (await res.getAttribute("data-keyword")) ?? "";
  expect(kw).toMatch(/^#[0-9A-F]{6}$/);
  expect(parseInt(kw.slice(1, 3), 16)).toBeGreaterThan(200);
  expect(parseInt(kw.slice(5, 7), 16)).toBeLessThan(80);
  const at = Number(/at (\d+) % height/.exec(text)![1]);
  expect(at).toBeGreaterThan(45);
  expect(at).toBeLessThan(65);
  // the style is in the gallery; a click applies it
  const tile = page.locator('[data-name="≈ h79-v4"]');
  await expect(tile).toHaveCount(1, { timeout: 60_000 });
  await tile.locator(".look-pick").click();
  await expect(page.locator('[data-name="≈ h79-v4"].current')).toHaveCount(1, { timeout: 60_000 });
  expect(errors).toEqual([]);
});
