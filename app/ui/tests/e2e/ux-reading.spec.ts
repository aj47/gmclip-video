// AJ 2026-10-16: "i need to be able to view the transcript (without times) in full". One
// click (Read, in the transcript's bar; View › Read the full transcript; ⌥R) shows the
// whole transcript as prose in a large pane beside the viewer: no times, paragraphs at
// pauses, large type that grows and shrinks, search inside it, a click on a word seeks,
// a selection still cuts, and "Only the cut" hides the words the cut does not play. The
// panel reads as prose too, and the words strip steps aside while it shows. Settings › Appearance sets the
// theme, the density, the text sizes and the panes a launch opens with.
import { expect, test, type Page } from "@playwright/test";
import { bridge, HAVE_VIDEO, VIDEO, onScreen, openPage, openVideo, overflow, shot } from "./ux-lib";
import { openTab } from "./tabs";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!HAVE_VIDEO, "no test video on this machine");

const frameOf = async (page: Page) => Number((await page.getByTestId("timecode").getAttribute("data-frame")) ?? NaN);

test("the full transcript reads as prose, and so does the panel", async ({ browser }) => {
  test.setTimeout(240_000);
  const { page, errors } = await openPage(browser);
  await openVideo(page);
  const pane = page.getByTestId("transcript-pane");
  const body = page.getByTestId("transcript");

  // The panel: prose, never times; the words strip steps aside while it shows.
  await openTab(page, "Transcript");
  await expect(body.locator(".tr-t").first()).toBeHidden();
  await expect(page.locator(".cut")).toHaveAttribute("data-words", "off");
  // the panel is prose too (2026-11-22): lines inline in block paragraphs, as in reading
  expect(await body.locator(".tr-line").first().evaluate((el) => getComputedStyle(el).display)).toBe("inline");
  expect(await body.locator(".tr-para").first().evaluate((el) => getComputedStyle(el).display)).toBe("block");
  await shot(page, "transcript-panel-no-times-1440x900");

  // One click: the reading view.
  await page.getByTestId("reading-open").click();
  await expect(pane).toHaveClass(/reading/);
  await expect(page.getByRole("toolbar", { name: "Reading" })).toBeVisible();
  await expect(body.locator(".tr-t").first()).toBeHidden();
  const vp = page.viewportSize()!;
  const pb = (await pane.boundingBox())!;
  expect(pb.width, "the reading pane is most of the window").toBeGreaterThan(vp.width * 0.5);
  expect(pb.height).toBeGreaterThan(vp.height * 0.7);
  await expect(page.locator(".timeline")).toBeHidden();
  await expect(page.locator(".viewer")).toBeVisible();
  const fs = async () => Number.parseFloat(await body.evaluate((el) => getComputedStyle(el).fontSize));
  expect(await fs(), "large type").toBeGreaterThanOrEqual(18);
  // prose: lines flow inline inside block paragraphs
  expect(await body.locator(".tr-line").first().evaluate((el) => getComputedStyle(el).display)).toBe("inline");
  expect(await body.locator(".tr-para").first().evaluate((el) => getComputedStyle(el).display)).toBe("block");
  expect(await overflow(page)).toEqual([]);
  await shot(page, "reading-view-1440x900");

  // Larger and smaller type (saved).
  const before = await fs();
  await page.getByTestId("read-larger").click();
  await page.getByTestId("read-larger").click();
  await expect.poll(fs).toBe(before + 4);
  await expect.poll(async () => (await bridge(page, "ui_prefs_get")).output.read_size).toBe(before + 4);
  await page.getByTestId("read-smaller").click();
  await expect.poll(fs).toBe(before + 2);

  // Search inside it.
  const search = page.getByRole("searchbox", { name: "Search the transcript" });
  await search.fill("banana");
  await expect(page.getByTestId("hits")).toContainText("1 /");
  await expect(body.locator(".w.hit").first()).toBeVisible();
  await search.fill("");

  // A click on a word seeks.
  await body.focus();
  await page.keyboard.press("Home");
  await expect.poll(() => frameOf(page)).toBe(0);
  const w = body.locator(".w:not(.out)").nth(40);
  await w.scrollIntoViewIfNeeded();
  await w.click();
  await expect.poll(() => frameOf(page), { timeout: 10_000 }).toBeGreaterThan(0);

  // A selection still cuts (and ⌘Z puts it back).
  const a = body.locator(".w:not(.out)").nth(10);
  const b = body.locator(".w:not(.out)").nth(12);
  await a.click();
  await b.click({ modifiers: ["Shift"] });
  const tools = page.getByRole("toolbar", { name: "Transcript actions" });
  await expect(tools).toContainText("3 words");
  const out0 = await body.locator(".w.out").count();
  await tools.getByRole("button", { name: "Cut", exact: true }).click();
  await expect.poll(() => body.locator(".w.out").count(), { timeout: 20_000 }).toBe(out0 + 3);
  // Only the cut: struck words go
  await page.getByTestId("reading-which").getByRole("button", { name: "Only the cut" }).click();
  await expect(body.locator(".w.out").first()).toBeHidden();
  await shot(page, "reading-only-the-cut-1440x900");
  await page.getByTestId("reading-which").getByRole("button", { name: "Whole source" }).click();
  await expect(body.locator(".w.out").first()).toBeVisible();
  await body.focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => body.locator(".w.out").count(), { timeout: 20_000 }).toBe(out0);

  // Esc drops the selection first, then leaves; ⌥R comes back; Done leaves.
  await body.focus();
  await page.keyboard.press("Escape");
  await expect(body.locator(".w.sel")).toHaveCount(0);
  await expect(pane).toHaveClass(/reading/);
  await page.keyboard.press("Escape");
  await expect(pane).not.toHaveClass(/reading/);
  await expect(page.locator(".timeline")).toBeVisible();
  await page.keyboard.press("Alt+KeyR");
  await expect(pane).toHaveClass(/reading/);
  // a small window reads too
  await page.setViewportSize({ width: 500, height: 1000 });
  await onScreen(page, "reading 500×1000: done", page.getByTestId("reading-done"));
  expect(await overflow(page)).toEqual([]);
  await page.getByTestId("reading-done").click();
  await expect(pane).not.toHaveClass(/reading/);
  expect(errors).toEqual([]);
});

test("Settings › Appearance and layout", async ({ browser }) => {
  test.setTimeout(180_000);
  const { page, errors } = await openPage(browser);
  await openVideo(page);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const sec = page.getByTestId("appearance");
  await expect(sec).toBeVisible();
  await shot(page, "settings-appearance-1440x900");
  // theme
  await page.getByTestId("pref-theme").getByRole("button", { name: "Light" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await shot(page, "settings-light-1440x900");
  await page.getByTestId("pref-theme").getByRole("button", { name: "Dark" }).click();
  await expect(page.locator("html")).not.toHaveAttribute("data-theme", "light");
  // density
  await page.getByTestId("pref-density").getByRole("button", { name: "Compact" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-density", "compact");
  const bar = async () => (await page.locator(".topbar").boundingBox())!.height;
  expect(await bar()).toBeLessThan(44);
  await page.getByTestId("pref-density").getByRole("button", { name: "Comfortable" }).click();
  await expect.poll(bar).toBe(44);
  // the transcript's text size
  await page.getByTestId("pref-tr-size").fill("18");
  await page.getByTestId("pref-open-panes").selectOption("focus");
  await expect.poll(async () => { const p = (await bridge(page, "ui_prefs_get")).output; return [p.tr_size, p.open_panes, p.density, p.theme]; }).toEqual([18, "focus", "comfortable", "dark"]);
  // back in the cut: the transcript uses them
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("Go to Home");
  await page.keyboard.press("Enter");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect.poll(async () => Number.parseFloat(await page.getByTestId("transcript").evaluate((el) => getComputedStyle(el).fontSize))).toBe(18);
  await expect(page.getByTestId("transcript").locator(".tr-t").first()).toBeHidden();
  // the panes a launch opens with: Reset layout from settings
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByTestId("pref-reset-layout").click();
  await expect.poll(async () => (await bridge(page, "ui_prefs_get")).output.side_open).toBe(true);
  expect(errors).toEqual([]);
});
