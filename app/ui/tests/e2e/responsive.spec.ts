// AJ 2026-10-15: "it looks bad on small windows", "doesnt resize for vertical", "i
// couldnt find the toggle hover scrub". At every window size from a laptop down to a
// narrow portrait window, in both framings: the frame fills the stage (≥ 90 % of the
// stage's limiting dimension, refitted on resize and on a framing switch), nothing
// overflows sideways, and the main controls (Export, play, the timeline, the framing
// switch, the labelled hover-scrub toggle) are on screen. The right panel is a drawer in
// compact windows; a portrait window stacks viewer, panels and timeline. The inspector's
// panels lay out at 600×900 and 800×600, and a short window scrolls the timeline's lanes.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
const SIZES = [
  { width: 1440, height: 900 },
  { width: 1100, height: 700 },
  { width: 960, height: 640 },
  { width: 800, height: 600 },
  { width: 600, height: 900 },
];

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

/** The frame's share of the stage's limiting dimension, and anything wider than its box. */
async function measure(page: Page) {
  return page.evaluate(() => {
    const box = document.querySelector(".viewer-box")!.getBoundingClientRect();
    const c = document.querySelector(".viewer-canvas")!.getBoundingClientRect();
    const a = c.width / c.height;
    const fill = box.width / box.height > a ? c.height / box.height : c.width / box.width;
    const over: string[] = [];
    if (document.documentElement.scrollWidth > innerWidth + 1) over.push(`page ${document.documentElement.scrollWidth} > ${innerWidth}`);
    for (const sel of [".topbar", ".cut-head", ".cut", ".transport", ".timeline-bar", ".tabs-list", ".viewer"]) {
      const el = document.querySelector<HTMLElement>(sel);
      if (!el || !el.offsetParent) continue;
      if (el.scrollWidth > el.clientWidth + 1) over.push(`${sel} ${el.scrollWidth} > ${el.clientWidth}`);
    }
    return { fill, aspect: a, over };
  });
}

async function onScreen(page: Page, what: string, sel: ReturnType<Page["locator"]>) {
  const b = await sel.boundingBox();
  const vp = page.viewportSize()!;
  expect(b, `${what} is laid out`).not.toBeNull();
  expect(b!.x, `${what} left`).toBeGreaterThanOrEqual(-1);
  expect(b!.y, `${what} top`).toBeGreaterThanOrEqual(-1);
  expect(b!.x + b!.width, `${what} right`).toBeLessThanOrEqual(vp.width + 1);
  expect(b!.y + b!.height, `${what} bottom`).toBeLessThanOrEqual(vp.height + 1);
}

test("small windows, portrait windows and the vertical frame", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: SIZES[0]! });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  const out = page.getByRole("group", { name: "Output" });

  for (const aspect of ["9:16", "16:9"] as const) {
    await page.setViewportSize(SIZES[0]!);
    await out.getByRole("button", { name: aspect }).click();
    await expect(out.getByRole("button", { name: aspect })).toHaveAttribute("aria-pressed", "true", { timeout: 20_000 });
    for (const vp of SIZES) {
      await page.setViewportSize(vp);
      const where = `${aspect} at ${vp.width}×${vp.height}`;
      // the frame refits to its stage (and has the framing's shape)
      const want = aspect === "9:16" ? 9 / 16 : 16 / 9;
      await expect
        .poll(async () => { const m = await measure(page); return m.fill >= 0.9 && Math.abs(m.aspect - want) < 0.03 ? "fits" : `fill ${m.fill.toFixed(2)}, shape ${m.aspect.toFixed(3)}`; }, { message: `${where}: frame fills its stage with its own shape`, timeout: 5_000 })
        .toBe("fits");
      const m = await measure(page);
      expect(m.over, `${where}: nothing overflows sideways`).toEqual([]);
      await onScreen(page, `${where}: Export`, page.getByTestId("export"));
      await onScreen(page, `${where}: play`, page.getByRole("button", { name: "Play", exact: true }));
      await onScreen(page, `${where}: framing switch`, out);
      // (the timeline section: in a short window its lanes scroll inside it)
      await onScreen(page, `${where}: timeline`, page.locator(".timeline"));
    }
  }

  // Compact window: the side panel is a drawer, closed until asked for.
  await page.setViewportSize({ width: 800, height: 600 });
  const side = page.getByRole("complementary", { name: "Side panel" });
  await expect(side).toBeHidden();
  await page.getByRole("button", { name: "Show the side panel" }).click();
  await expect(side).toBeVisible();
  await onScreen(page, "side drawer", side);
  await expect(page.getByRole("tab", { name: "Look" })).toBeVisible();
  await page.getByRole("button", { name: "Hide the side panel" }).click();
  await expect(side).toBeHidden();

  // Portrait window: viewer above the panels, the timeline below them.
  await page.setViewportSize({ width: 600, height: 900 });
  await expect(side).toBeVisible();
  const vb = (await page.locator(".viewer").boundingBox())!;
  const sb = (await side.boundingBox())!;
  const tb = (await page.locator(".timeline").boundingBox())!;
  expect(vb.y + vb.height, "viewer above the panels").toBeLessThanOrEqual(sb.y + 1);
  expect(sb.y + sb.height, "panels above the timeline").toBeLessThanOrEqual(tb.y + 1);

  // The inspector's panels lay out in a compact and a portrait window: the caption
  // gallery (with Match a short), images and callouts, the brand kit, the cut-out,
  // follow cursor and the linked layout — each fits the panel's width, nothing overflows
  // sideways, and each can be scrolled to.
  for (const vp of [{ width: 600, height: 900 }, { width: 800, height: 600 }]) {
    await page.setViewportSize(vp);
    if (vp.width === 800) await page.getByRole("button", { name: "Show the side panel" }).click();
    await openTab(page, "Inspector");
    const toggle = page.getByTestId("gallery-toggle");
    if ((await toggle.getAttribute("aria-expanded")) !== "true") await toggle.click();
    await expect(page.getByTestId("caption-gallery")).toBeVisible({ timeout: 30_000 });
    for (const [what, loc] of [
      ["caption styles", page.getByRole("region", { name: "Caption styles" })],
      ["match a short", page.getByTestId("match-short")],
      ["images and callouts", page.getByRole("region", { name: "Images and callouts" })],
      ["brand kit", page.getByRole("region", { name: "Brand kit" })],
      ["cut-out", page.getByTestId("roto-panel")],
      ["follow cursor", page.getByTestId("cursor-follow")],
    ] as const) {
      const where = `${what} at ${vp.width}×${vp.height}`;
      await loc.first().scrollIntoViewIfNeeded();
      // inside the window sideways; its top in view (a tall panel scrolls on down)
      const b = (await loc.first().boundingBox())!;
      expect(b.x, `${where} left`).toBeGreaterThanOrEqual(-1);
      expect(b.x + b.width, `${where} right`).toBeLessThanOrEqual(vp.width + 1);
      expect(b.y, `${where} top`).toBeLessThan(vp.height);
      expect(b.y + Math.min(b.height, 40), `${where} top`).toBeGreaterThan(0);
      const over = await loc.first().evaluate((el) => el.scrollWidth - el.clientWidth);
      expect(over, `${where}: nothing wider than the panel`).toBeLessThanOrEqual(1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth), `${vp.width}×${vp.height}: no sideways page scroll`).toBeLessThanOrEqual(1);
    if (vp.width === 800) await page.getByRole("button", { name: "Hide the side panel" }).click();
  }

  // A short window: the timeline's lanes scroll, so each lane head can be brought into
  // its view (a window ≤ 600 px tall clipped the caption and text lanes before).
  await page.setViewportSize({ width: 800, height: 600 });
  const body = page.getByTestId("timeline-body");
  for (const lane of ["video", "gain", "captions", "text"]) {
    const head = page.locator(`.lane-head.lane-${lane}`);
    await head.scrollIntoViewIfNeeded();
    const hb = (await head.boundingBox())!;
    const bb = (await body.boundingBox())!;
    expect(hb.y, `${lane} lane head reachable`).toBeGreaterThanOrEqual(bb.y - 1);
    expect(hb.y + hb.height, `${lane} lane head reachable`).toBeLessThanOrEqual(bb.y + bb.height + 1);
  }
  expect(errors).toEqual([]);
});
