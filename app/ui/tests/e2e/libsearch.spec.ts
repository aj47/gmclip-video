// L.01: search everything said across the library. A stream's project fixture is opened (its
// transcript comes with it), ⌘⇧F opens search from anywhere, a phrase finds the words,
// ▶ plays them, ＋ makes a moment, Home's search box lands on the same view, and the view
// fits a phone-width window. Needs the lab media (read-only); skips without it.
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { hasLab, openLabProject } from "./lab";


test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!hasLab("preview"), "no lab media on this machine");

test("search the library, play a hit, make a moment", async ({ browser }) => {
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await openLabProject(page, "loc_4fadb756");
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<{ output?: unknown }> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  const open = (await bridge("project_list_open", {})).output as { id: string; name: string }[];
  const pid = open.find((p) => p.name.includes("loc_4fadb756"))?.id ?? open[0]!.id;
  const count = async () => ((await bridge("moments_list", {}, pid)).output as { moments: { origin: string; type: string }[] }).moments;
  const before = (await count()).length;

  // ⌘⇧F from the cut
  await page.keyboard.press("ControlOrMeta+Shift+F");
  const view = page.getByTestId("library-search");
  await expect(view).toBeVisible();
  await page.getByRole("button", { name: "Words" }).click();
  await page.getByTestId("library-search-input").fill("generate that five second video");
  const hit = page.getByTestId("library-hit").first();
  await expect(hit).toBeVisible({ timeout: 30_000 });
  await expect(hit.locator("mark")).toHaveText(/generate that five.second video/i);
  await expect(hit).toContainText("loc_4fadb756");

  // ▶ plays the words
  await hit.getByTestId("hit-play").click();
  await expect(page.getByTestId("viewer-stats")).toContainText("×", { timeout: 30_000 });
  const tc = page.getByTestId("timecode");
  const t0 = await tc.textContent();
  await page.waitForTimeout(800);
  expect(await tc.textContent()).not.toBe(t0);

  // ＋ makes a word-edged moment by you
  await hit.getByTestId("hit-moment").click();
  await expect(page.getByText(/Moment added to/)).toBeVisible();

  // Home's box lands on the same view with the words in it
  await page.getByRole("button", { name: "Home" }).click();
  await page.getByTestId("home-search").getByRole("searchbox").fill("five second video");
  await page.getByTestId("home-search").getByRole("button", { name: "Search" }).click();
  await expect(page.getByTestId("library-search-input")).toHaveValue("five second video");
  await expect(page.getByTestId("library-hit").first()).toBeVisible({ timeout: 30_000 });

  // a phone-width window: no sideways scroll, the hit's buttons still reachable
  await page.setViewportSize({ width: 390, height: 800 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await expect(page.getByTestId("library-hit").first().getByTestId("hit-moment")).toBeInViewport();

  // the new moment is in the project's list, labelled as yours
  await page.setViewportSize({ width: 1440, height: 900 });
  const ms = await count();
  expect(ms.length).toBe(before + 1);
  expect(ms.filter((m) => m.origin === "manual" && m.type === "search").length).toBe(1);
  expect(errors).toEqual([]);
});
