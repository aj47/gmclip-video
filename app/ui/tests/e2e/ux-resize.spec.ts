// AJ 2026-10-16: "the window should be resizable. right now i cant reduce it below a
// certain width" (the window's minimum is now 480 × 360). Every view at every size from
// 480 × 360 to 1440 × 900, a tall narrow 500 × 1000 included: no sideways scroll, nothing
// wider than its bar, the main controls inside the window, the viewer a usable size, and
// in the cut view every pane toggles from the View menu. Screenshots for AJ go to
// milestones/m2/ui/ux-2026-10-16/ with GMCLIP_UPDATE_SHOTS=1 on a Mac (ux-lib's SHOTS).
import { expect, test, type Page } from "@playwright/test";
import { HAVE_VIDEO, onScreen, openPage, openVideo, overflow, shot, SIZES, viewItem } from "./ux-lib";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!HAVE_VIDEO, "no test video on this machine");

const name = (vp: { width: number; height: number }) => `${vp.width}x${vp.height}`;

async function palette(page: Page, what: string) {
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill(what);
  await page.keyboard.press("Enter");
}

/** Each pane in the View menu: its tick flips, the pane hides, and back. */
async function panesToggle(page: Page, where: string) {
  const cut = page.getByRole("main", { name: "Cut" });
  const checks: [string, string, () => Promise<boolean>][] = [
    ["view-side", "side", () => page.getByRole("complementary", { name: "Side panel" }).isVisible()],
    ["view-timeline", "tl", () => page.locator(".timeline").isVisible()],
    ["view-map", "map", () => page.getByRole("complementary", { name: "Project map sidebar" }).isVisible()],
  ];
  for (const [id, attr, visible] of checks) {
    const item = await viewItem(page, id);
    const was = (await item.getAttribute("aria-checked")) === "true";
    await item.click();
    if (attr !== "map") await expect(cut, `${where}: ${id} flips`).toHaveAttribute(`data-${attr}`, was ? "off" : "on");
    await expect.poll(visible, { message: `${where}: ${id} ${was ? "hides" : "shows"}` }).toBe(!was);
    expect(await overflow(page), `${where}: ${id} toggled, nothing overflows`).toEqual([]);
    await (await viewItem(page, id)).click();
    await expect.poll(visible, { message: `${where}: ${id} back` }).toBe(was);
  }
}

test("every view at every window size", async ({ browser }) => {
  test.setTimeout(420_000);
  const { page, errors } = await openPage(browser);
  await openVideo(page);

  // The cut view.
  for (const vp of SIZES) {
    await page.setViewportSize(vp);
    const where = `cut ${name(vp)}`;
    await expect.poll(() => overflow(page), { message: `${where}: nothing overflows sideways`, timeout: 5_000 }).toEqual([]);
    await onScreen(page, `${where}: Export`, page.getByTestId("export"));
    await onScreen(page, `${where}: View menu`, page.getByTestId("view-menu"));
    await onScreen(page, `${where}: play`, page.getByRole("button", { name: "Play", exact: true }));
    await onScreen(page, `${where}: side panel toggle`, page.locator(".side-toggle"));
    await onScreen(page, `${where}: timeline`, page.locator(".timeline"));
    const v = (await page.locator(".viewer-canvas").boundingBox())!;
    expect(Math.min(v.width, v.height), `${where}: the picture is a usable size`).toBeGreaterThanOrEqual(vp.height < 400 ? 70 : 120);
    await shot(page, `cut-${name(vp)}`);
    await panesToggle(page, where);
  }

  // Reading and focus at the small sizes too.
  for (const vp of SIZES) {
    await page.setViewportSize(vp);
    const where = `reading ${name(vp)}`;
    // reading is ⌥R (and ⌘K) since decision 0054, not the View menu
    await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
    await page.keyboard.press("Alt+KeyR");
    await expect(page.getByRole("toolbar", { name: "Reading" })).toBeVisible();
    await expect.poll(() => overflow(page), { message: `${where}: nothing overflows`, timeout: 5_000 }).toEqual([]);
    await onScreen(page, `${where}: done`, page.getByTestId("reading-done"));
    await onScreen(page, `${where}: search`, page.getByRole("searchbox", { name: "Search the transcript" }));
    await shot(page, `reading-${name(vp)}`);
    await page.getByTestId("reading-done").click();
    await (await viewItem(page, "view-focus")).click();
    await expect(page.getByRole("main", { name: "Cut" })).toHaveAttribute("data-focus", "on");
    await expect.poll(() => overflow(page)).toEqual([]);
    await shot(page, `focus-${name(vp)}`);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("main", { name: "Cut" })).not.toHaveAttribute("data-focus", "on");
  }

  // Search, settings and Home.
  const views: [string, () => Promise<void>, () => ReturnType<Page["locator"]>][] = [
    ["search", () => page.keyboard.press("ControlOrMeta+Shift+f"), () => page.getByRole("main", { name: "Search the library" })],
    ["settings", () => page.getByRole("button", { name: "Settings", exact: true }).click(), () => page.getByRole("main", { name: "Settings" })],
    ["home", () => page.getByRole("button", { name: "Home", exact: true }).click(), () => page.locator(".home")],
  ];
  for (const [view, go, main] of views) {
    await page.setViewportSize({ width: 1440, height: 900 });
    await go();
    await expect(main()).toBeVisible();
    for (const vp of SIZES) {
      await page.setViewportSize(vp);
      const where = `${view} ${name(vp)}`;
      await expect.poll(() => overflow(page), { message: `${where}: nothing overflows sideways`, timeout: 5_000 }).toEqual([]);
      await onScreen(page, `${where}: Home button`, page.getByRole("button", { name: "Home", exact: true }));
      const m = (await main().boundingBox())!;
      expect(m.x + m.width, `${where}: the view fits the window`).toBeLessThanOrEqual(vp.width + 1);
      await shot(page, `${view}-${name(vp)}`);
    }
  }
  expect(errors).toEqual([]);
});
