// Decision 0010 (amends R-FRAME-1): a frame socket closed because the window stayed hidden
// comes back by itself when the window shows: a fresh token, one new connection, the
// current frame redrawn. Dev builds drive the hide → close → show cycle directly.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("the preview reconnects after a long hide", async ({ browser }) => {
  test.setTimeout(120_000);
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
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  const stats = page.getByTestId("viewer-stats");
  await expect(page.locator(".viewer-empty")).toHaveCount(0, { timeout: 30_000 });
  const cycle = page.evaluate(() =>
    (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<unknown> }).__gmclipBridgeCall("dev_frame_cycle", {}),
  );
  // closed, then live again by itself
  await expect(page.locator(".viewer-empty")).toContainText("paused", { timeout: 10_000 });
  expect(await cycle).toMatchObject({ output: { rearmed: true } });
  await expect(page.locator(".viewer-empty")).toHaveCount(0, { timeout: 15_000 });
  // frames flow again: a step draws the stepped frame (the stats line carries the output
  // frame last drawn; its text only says size, rate and render time, which a step need
  // not change)
  await expect(stats).toHaveAttribute("data-frame", /^\d+$/, { timeout: 10_000 });
  const before = Number(await stats.getAttribute("data-frame"));
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.10/, { timeout: 10_000 });
  const stepped = Number(await page.getByTestId("timecode").getAttribute("data-frame"));
  expect(stepped).not.toBe(before);
  await expect(stats).toHaveAttribute("data-frame", String(stepped), { timeout: 10_000 });
  expect(errors).toEqual([]);
});
