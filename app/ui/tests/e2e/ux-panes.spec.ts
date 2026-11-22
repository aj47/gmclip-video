// AJ 2026-10-16: "hide the sidebar, the ui should be very configurable". Every pane
// shows and hides from one View menu (tick + key), from its ⌥ key and from its own
// button, in every layout including portrait; focus keeps only the viewer and the
// timeline; the side panel, the map and the timeline resize on splitters (double-click
// resets); all of it is saved in the UI preferences and survives a reload; Reset layout
// brings everything back. The keys are in the ? list.
import { expect, test, type Page } from "@playwright/test";
import { bridge, HAVE_VIDEO, VIDEO, onScreen, openPage, openVideo, overflow, shot, viewItem } from "./ux-lib";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!HAVE_VIDEO, "no test video on this machine");

const prefs = async (page: Page) => (await bridge(page, "ui_prefs_get")).output;
const width = async (page: Page, sel: string) => (await page.locator(sel).boundingBox())?.width ?? 0;
const height = async (page: Page, sel: string) => (await page.locator(sel).boundingBox())?.height ?? 0;

async function drag(page: Page, testid: string, dx: number, dy: number) {
  const b = (await page.getByTestId(testid).boundingBox())!;
  const x = b.x + b.width / 2;
  const y = b.y + b.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y + dy, { steps: 8 });
  await page.mouse.up();
}

test("panes hide, show, resize and are remembered", async ({ browser }) => {
  test.setTimeout(300_000);
  const { page, errors } = await openPage(browser);
  await openVideo(page);
  const cut = page.getByRole("main", { name: "Cut" });
  const side = page.getByRole("complementary", { name: "Side panel" });
  const map = page.getByRole("complementary", { name: "Project map sidebar" });
  const tl = page.locator(".timeline");
  const words = page.locator(".words");

  // The View menu lists every pane with its key.
  await (await viewItem(page, "view-side")).isVisible();
  const menu = page.getByRole("menu", { name: "View" });
  for (const [label, keys] of [["Project map", "⌥1"], ["Side panel", "⌥2"], ["Timeline", "⌥3"], ["Words", "⌥T"], ["Look", "⌥I"], ["Focus: viewer and timeline", "⌥F"], ["Hover scrub", "⌥S"], ["Reset layout", "⌥0"]]) {
    // (the words strip retired 2026-11-22: "Words" is the tab)
    const name = label === "Words" ? /^Words/ : new RegExp(`^${label.replace(/[()]/g, "\\$&")}`);
    await expect(menu.getByRole(/Reset/.test(label) ? "menuitem" : "menuitemcheckbox", { name })).toContainText(process.platform === "darwin" ? keys : keys.replace("⌥", "Alt+"));
  }
  await shot(page, "view-menu-1440x900");
  await page.keyboard.press("Escape");

  // Wide: each pane from the menu, then from its key.
  await expect(side).toBeVisible();
  await expect(map).toBeVisible();
  await (await viewItem(page, "view-side")).click();
  await expect(side).toBeHidden();
  await expect(cut).toHaveAttribute("data-side", "off");
  await page.keyboard.press("Alt+Digit2");
  await expect(side).toBeVisible();
  await page.keyboard.press("Alt+Digit1");
  await expect(map).toBeHidden();
  await page.keyboard.press("Alt+Digit1");
  await expect(map).toBeVisible();
  await page.keyboard.press("Alt+Digit3");
  await expect(tl).toBeHidden();
  // the stage takes the room
  const vh = await height(page, ".viewer");
  await page.keyboard.press("Alt+Digit3");
  await expect(tl).toBeVisible();
  expect(await height(page, ".viewer")).toBeLessThan(vh);
  // the words strip is retired (AJ 2026-11-22): ⌥4, its key, shows the transcript's tab
  await expect(words).toBeHidden();
  // from another tab (⌥4 on the Words tab hides the panel, as ⌥T does)
  await page.getByRole("tab", { name: "Captions" }).click();
  await page.keyboard.press("Alt+Digit4");
  await expect(page.getByRole("tab", { name: "Words" })).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Alt+Digit4");
  await expect(side).toBeHidden();
  await page.keyboard.press("Alt+Digit2");
  await expect(side).toBeVisible();
  await page.getByRole("tab", { name: "Captions" }).click();
  // the transcript and the inspector as panes of their own
  await page.keyboard.press("Alt+KeyT");
  await expect(page.getByRole("tab", { name: "Words" })).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Alt+KeyT");
  await expect(side).toBeHidden();
  await page.keyboard.press("Alt+KeyI");
  await expect(side).toBeVisible();
  await expect(page.getByRole("tab", { name: "Look" })).toHaveAttribute("aria-selected", "true");

  // Focus: the viewer and the timeline only; a pane asked for leaves it.
  await page.keyboard.press("Alt+KeyF");
  await expect(cut).toHaveAttribute("data-focus", "on");
  await expect(side).toBeHidden();
  await expect(map).toBeHidden();
  await expect(words).toBeHidden();
  await expect(tl).toBeVisible();
  await expect(page.locator(".viewer")).toBeVisible();
  await shot(page, "focus-mode-1440x900");
  await page.keyboard.press("Alt+Digit2");
  await expect(cut).not.toHaveAttribute("data-focus", "on");
  await expect(side).toBeVisible();
  await (await viewItem(page, "view-focus")).click();
  await expect(cut).toHaveAttribute("data-focus", "on");
  await page.keyboard.press("Escape");
  await expect(cut).not.toHaveAttribute("data-focus", "on");

  // Splitters: the side panel wider, the map wider, the timeline taller.
  const sw = await width(page, ".side");
  await drag(page, "split-side", -160, 0);
  await expect.poll(() => width(page, ".side")).toBeGreaterThan(sw + 120);
  const mw = await width(page, ".map");
  await drag(page, "split-map", 80, 0);
  await expect.poll(() => width(page, ".map")).toBeGreaterThan(mw + 50);
  const th = await height(page, ".timeline");
  await drag(page, "split-tl", 0, -120);
  await expect.poll(() => height(page, ".timeline")).toBeGreaterThan(th + 80);
  expect(await overflow(page)).toEqual([]);
  await shot(page, "resized-panes-1440x900");
  // saved: they come back after a reload
  await expect.poll(async () => { const p = await prefs(page); return [Number(p.side_w) > 0, Number(p.map_w) > 0, Number(p.tl_h) > 0]; }).toEqual([true, true, true]);
  const sw2 = await width(page, ".side");
  await page.reload();
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect.poll(() => width(page, ".side")).toBeGreaterThan(sw2 - 3);
  await expect(words).toBeHidden();
  // double-click resets one splitter; Reset layout everything
  await page.getByTestId("split-side").dblclick();
  await expect.poll(() => width(page, ".side")).toBeLessThan(sw2 - 100);
  await (await viewItem(page, "view-reset")).click();
  await expect(words).toBeHidden();
  await expect.poll(async () => { const p = await prefs(page); return [p.side_w, p.map_w, p.tl_h, p.words_open, p.side_open]; }).toEqual([0, 0, 0, true, true]);

  // Portrait: the side panel hides and shows too (it used to be forced open).
  await page.setViewportSize({ width: 600, height: 900 });
  await expect(cut).toHaveAttribute("data-layout", "portrait");
  await expect(side).toBeVisible();
  await page.getByRole("button", { name: "Hide the side panel" }).click();
  await expect(side).toBeHidden();
  await onScreen(page, "portrait: timeline", tl);
  expect(await overflow(page)).toEqual([]);
  await shot(page, "portrait-side-hidden-600x900");
  await (await viewItem(page, "view-side")).click();
  await expect(side).toBeVisible();
  await page.keyboard.press("Alt+Digit3");
  await expect(tl).toBeHidden();
  await page.keyboard.press("Alt+Digit3");
  await expect(tl).toBeVisible();
  // the map is a drawer there
  await page.getByRole("button", { name: "Show the project map" }).click();
  await expect(map).toBeVisible();
  await onScreen(page, "portrait: map drawer", map);
  await page.getByRole("button", { name: "Hide the project map" }).first().click();
  await expect(map).toBeHidden();

  // Compact: the side panel is a drawer; View opens it.
  await page.setViewportSize({ width: 800, height: 600 });
  await expect(side).toBeHidden();
  await (await viewItem(page, "view-side")).click();
  await expect(side).toBeVisible();
  await onScreen(page, "compact: side drawer", side);
  await page.keyboard.press("Alt+Digit2");
  await expect(side).toBeHidden();

  // The keys are in the ? list.
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.getByTestId("transcript").focus();
  await page.keyboard.press("?");
  const help = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  // the essentials first; the groups behind "Show all keys" (2026-11-22)
  await help.getByTestId("keys-all").click();
  await expect(help.getByRole("heading", { name: "View" })).toBeVisible();
  await expect(help).toContainText("Read the full transcript");
  await expect(help).toContainText("Focus: only the viewer and the timeline");
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});
