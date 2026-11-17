// AJ 2026-10-19: the project map's toggle at the far left of the top bar (decision 0054:
// the only one on screen, no tab on the left edge); Agents keeps the project in the top bar;
// layouts the person saved before decision 0054 are still in the View menu (none built in,
// none saved from it), a saved side tab by its old name opening its new tab (Short is
// Captions) and a tab off the bar opening Words; layout presets saved, renamed, updated.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";
import { bridge } from "./ux-lib";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("map toggle, the project on Agents, workspace layouts", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });

  // the map toggle is the top bar's first control, and the only one on screen
  const toggle = page.getByTestId("map-toggle");
  const first = await page.locator(".topbar > *").first().getAttribute("data-testid");
  expect(first).toBe("map-toggle");
  const map = page.getByRole("complementary", { name: "Project map sidebar" });
  await expect(map).toBeVisible();
  await toggle.click();
  await expect(map).toBeHidden();
  await expect(page.getByTestId("map-handle")).toHaveCount(0);
  await toggle.click();
  await expect(map).toBeVisible();

  // Agents keeps the project: its name, back to the cut
  await page.getByTestId("nav-agents").click();
  await expect(page.getByTestId("tb-project")).toContainText("own-vm");
  await page.getByTestId("tb-project").click();
  await expect(page.getByTestId("cut-title")).toContainText("own-vm");
  await expect(page.getByTestId("tb-project")).toHaveCount(0);

  // none built in, none saved from the menu
  await page.getByTestId("view-menu").click();
  await expect(page.locator("[data-testid^=view-layout]")).toHaveCount(0);
  await page.keyboard.press("Escape");
  // layouts saved before decision 0054 (an older ui-prefs.json): listed and used; "short"
  // opens Captions, "review" (off the bar) opens Words
  await bridge(page, "ui_prefs_set", {
    workspaces: [
      { name: "Trim", map_open: false, tl_h: 420, side_open: true, side_tab: "short" },
      { name: "Notes", map_open: true, tl_h: 0, side_open: true, side_tab: "review" },
    ],
  });
  await page.reload();
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await page.getByTestId("view-menu").click();
  await expect(page.getByTestId("view-layout-trim")).toHaveAttribute("aria-checked", "false");
  await page.getByTestId("view-layout-trim").click();
  await expect(map).toBeHidden();
  await expect(page.locator(".cut")).toHaveAttribute("data-tl-h", "1");
  await expect(page.getByRole("tab", { name: "Captions", exact: true })).toHaveAttribute("aria-selected", "true");
  await page.getByTestId("view-menu").click();
  await page.getByTestId("view-layout-notes").click();
  await expect(map).toBeVisible();
  await expect(page.getByRole("tab", { name: "Words", exact: true })).toHaveAttribute("aria-selected", "true");

  // video layouts on a 16:9 cut: a newer built-in, then saved, renamed and updated
  await openTab(page, "Layout");
  await page.getByRole("button", { name: "Screen + camera corner" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + camera corner 16:9", { timeout: 20_000 });
  await page.getByTestId("preset-name").fill("corner cam");
  await page.getByTestId("preset-save").click();
  await expect(page.getByTestId("preset-pick")).toHaveValue("corner cam");
  await page.getByTestId("preset-rename").fill("my corner");
  await page.getByTestId("preset-rename-go").click();
  await expect(page.getByTestId("preset-pick")).toHaveValue("my corner");
  await page.getByTestId("preset-update").click();
  await expect(page.locator(".toast")).toContainText("my corner is now part 1");
  await page.getByRole("button", { name: "Delete the preset my corner" }).click();
  await expect(page.getByTestId("preset-pick").locator("option", { hasText: "my corner" })).toHaveCount(0);
  expect(errors).toEqual([]);
});
