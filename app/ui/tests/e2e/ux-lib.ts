// Shared by the ux-* specs (AJ 2026-10-16: small windows, hideable panes, the reading
// view): open the test video in a fresh core, measure sideways overflow, check a control
// is inside the window, and take a screenshot for AJ.
import { expect, type Browser, type Locator, type Page } from "@playwright/test";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

export const VIDEO = labMedia("h79/h79-v4.mp4");
export const HAVE_VIDEO = existsSync(VIDEO);
/** Where the screenshots go: the committed set for AJ (milestones/m2/ui/ux-2026-10-16)
 * only with GMCLIP_UPDATE_SHOTS=1 (on a Mac, which drew them; a run elsewhere would
 * rewrite them with another platform's fonts), or the folder GMCLIP_SHOTS_DIR names. A
 * gate run takes none: nobody looks at them, and the tree stays clean. */
export const SHOTS =
  process.env.GMCLIP_UPDATE_SHOTS === "1" && process.platform === "darwin"
    ? resolve("../../milestones/m2/ui/ux-2026-10-16")
    : process.env.GMCLIP_SHOTS_DIR;

export const SIZES = [
  { width: 480, height: 360 },
  { width: 640, height: 480 },
  { width: 800, height: 600 },
  { width: 500, height: 1000 },
  { width: 1100, height: 700 },
  { width: 1440, height: 900 },
];

export async function openPage(browser: Browser, viewport = { width: 1440, height: 900 }) {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  return { page, errors };
}

/** Opens the test video as a cut (and waits for its words). */
export async function openVideo(page: Page) {
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect(page.getByTestId("transcript").locator(".w").first()).toBeAttached({ timeout: 120_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
}

/** Everything wider than its box: the page, and the main bars. */
export async function overflow(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const over: string[] = [];
    if (document.documentElement.scrollWidth > innerWidth + 1) over.push(`page ${document.documentElement.scrollWidth} > ${innerWidth}`);
    for (const sel of [".topbar", ".cut-head", ".cut", ".transport", ".timeline-bar", ".tr-bar", ".tr-readbar", ".app-body > main"]) {
      for (const el of document.querySelectorAll<HTMLElement>(sel)) {
        if (!el.offsetParent && getComputedStyle(el).position !== "fixed") continue;
        if (el.scrollWidth > el.clientWidth + 1 && getComputedStyle(el).overflowX !== "auto" && getComputedStyle(el).overflowX !== "scroll") over.push(`${sel} ${el.scrollWidth} > ${el.clientWidth}`);
      }
    }
    return over;
  });
}

/** The control is laid out and wholly inside the window. */
export async function onScreen(page: Page, what: string, loc: Locator) {
  const b = await loc.boundingBox();
  const vp = page.viewportSize()!;
  expect(b, `${what} is laid out`).not.toBeNull();
  expect(b!.width, `${what} has a width`).toBeGreaterThan(4);
  expect(b!.x, `${what} left`).toBeGreaterThanOrEqual(-1);
  expect(b!.y, `${what} top`).toBeGreaterThanOrEqual(-1);
  expect(b!.x + b!.width, `${what} right`).toBeLessThanOrEqual(vp.width + 1);
  expect(b!.y + b!.height, `${what} bottom`).toBeLessThanOrEqual(vp.height + 1);
}

export async function shot(page: Page, name: string) {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

/** The View menu's item (opened first). */
export async function viewItem(page: Page, testid: string) {
  const trigger = page.getByTestId("view-menu");
  if ((await trigger.getAttribute("aria-expanded")) !== "true") await trigger.click();
  return page.getByTestId(testid);
}

export const bridge = (page: Page, command: string, args: Record<string, unknown> = {}) =>
  page.evaluate(
    ([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: unknown) => Promise<{ output: Record<string, unknown> }> }).__gmclipBridgeCall(c as string, a),
    [command, args] as const,
  );

/** Settings › Developer on (decision 0054: the team and in-app agent surfaces, every side
 * panel in the bar), written as its toggle would; the page reloads to read it. */
export async function developerOn(page: Page) {
  await bridge(page, "ui_prefs_set", { developer: true });
  await page.reload();
  await expect(page.locator("html[data-booted='1']")).toBeAttached({ timeout: 30_000 });
}
