// Follow cursor (SPEC §5.7) on a screen recording opened as a video: the switch finds
// the cursor and writes the zoomed crop path (eased visual splits, so the cut's parts
// show the moves), ⌘Z takes the whole follow away in one step, and off removes it.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("follow the cursor on a part", async ({ browser }) => {
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
  await openTab(page, "Inspector");
  const sw = page.getByTestId("follow-cursor");
  await expect(sw).not.toBeChecked();
  const t0 = Date.now();
  await sw.check();
  await expect(page.getByTestId("follow-busy")).toHaveCount(0, { timeout: 180_000 });
  const toast = await page.locator(".toast").textContent();
  console.log(`follow: ${Date.now() - t0} ms — ${toast}`);
  await expect(sw).toBeChecked({ timeout: 20_000 });
  const moves = Number((await page.getByTestId("follow-moves").textContent())!.split(" ")[0]);
  expect(moves).toBeGreaterThan(0);
  // the moves are plan parts of their own on the timeline
  const rows = page.getByRole("list", { name: "Parts" }).getByRole("button");
  await expect.poll(() => rows.count()).toBe(moves + 1);
  if (process.env.CURSOR_SHOTS) {
    for (const k of [2, 5, 9]) {
      await rows.nth(k).dispatchEvent("click");
      await page.waitForTimeout(1500);
      await page.locator(".viewer-canvas").screenshot({ path: `${process.env.CURSOR_SHOTS}/move-${k}.png` });
    }
  }
  // ⌘Z: the whole follow in one step
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(sw).not.toBeChecked({ timeout: 20_000 });
  await expect.poll(() => rows.count()).toBe(1);
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect(sw).toBeChecked({ timeout: 20_000 });
  // off
  await sw.uncheck();
  await expect(sw).not.toBeChecked({ timeout: 60_000 });
  await expect.poll(() => rows.count()).toBe(1);
  expect(errors).toEqual([]);
});
