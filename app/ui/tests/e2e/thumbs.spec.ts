// QA (milestones/m2/qa-e2e.md, W4): thumbnail versions on the Thumbnail tab: two frames
// with titles, the other one picked (one picked at a time), and a PNG rendered.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("thumbnail versions: add two, pick the other, render", async ({ browser }) => {
  test.setTimeout(180_000);
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
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  const bridge = (c: string, a: Record<string, unknown>) =>
    page.evaluate(([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<unknown> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>), [c, a] as const);
  await openTab(page, "Thumbnail");
  for (const [f, t] of [[30, "First *one*"], [600, "Second *one*"]] as const) {
    await bridge("viewer_seek", { frame: f });
    await expect(page.getByTestId("timecode")).toHaveAttribute("data-frame", String(f));
    await page.getByTestId("thumb-title").fill(t);
    await page.getByTestId("thumb-use-frame").click();
    // the accent word is shown coloured, never with its stars
    const row = page.getByTestId("thumb").filter({ hasText: t.replace(/\*/g, "") });
    await expect(row).toHaveCount(1, { timeout: 20_000 });
    await expect(row.locator(".thread-text")).not.toContainText("*");
    await expect(row.locator(".thread-text .title-accent")).toHaveText("one");
  }
  const thumbs = page.getByTestId("thumb");
  await expect(thumbs).toHaveCount(2);
  await expect(page.locator('[data-testid="thumb"][data-picked="true"]')).toHaveCount(1);
  const other = page.locator('[data-testid="thumb"][data-picked="false"]');
  const title = ((await other.textContent()) ?? "").includes("Second") ? "Second" : "First";
  await other.getByTestId("thumb-pick").click();
  await expect(thumbs.filter({ hasText: title })).toHaveAttribute("data-picked", "true", { timeout: 20_000 });
  await expect(page.locator('[data-testid="thumb"][data-picked="true"]')).toHaveCount(1);
  await thumbs.filter({ hasText: title }).getByTestId("thumb-render").click();
  await expect(page.getByTestId("thumb-rendered")).toBeVisible({ timeout: 60_000 });
  expect(errors).toEqual([]);
});
