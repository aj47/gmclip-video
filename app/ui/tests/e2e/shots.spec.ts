// Before/after screenshots of the UI for AJ (milestones/m2/ui/): Home, a video opened
// straight from a file in both framings (9:16 and 16:9), and an imported RT plan
// (overlay, gain and caption lanes), at desktop, small-window and narrow sizes. Runs only
// with GMCLIP_SHOTS set:
//   GMCLIP_SHOTS=after-2 pnpm exec playwright test tests/e2e/shots.spec.ts
// writes milestones/m2/ui/<label>-<view>-<w>x<h>.png.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labMedia, labPlan } from "./lab";

const LABEL = process.env.GMCLIP_SHOTS ?? "";
const OUT = resolve("../../milestones/m2/ui");
const VIDEO = labMedia("h79/h79-v4.mp4");
const H79 = labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json"));
const SIZES = [
  { width: 1440, height: 900 },
  { width: 1100, height: 700 },
  { width: 960, height: 640 },
  { width: 800, height: 600 },
  { width: 600, height: 900 },
];

test.skip(!LABEL, "screenshots only with GMCLIP_SHOTS=<label>");
test.skip(!existsSync(VIDEO), "no test video on this machine");

async function shoot(page: Page, view: string) {
  // the pointer off every control (no tooltip in the picture)
  await page.mouse.move(1, 300);
  for (const vp of SIZES) {
    await page.setViewportSize(vp);
    await page.waitForTimeout(800);
    await page.screenshot({ path: `${OUT}/${LABEL}-${view}-${vp.width}x${vp.height}.png` });
  }
  await page.setViewportSize(SIZES[0]!);
  await page.waitForTimeout(300);
}

/** The framing switch (16:9 / 9:16) in the top bar. */
async function framing(page: Page, aspect: "16:9" | "9:16") {
  const b = page.getByRole("group", { name: "Output" }).getByRole("button", { name: aspect });
  if (!(await b.count())) return;
  if ((await b.getAttribute("aria-pressed")) !== "true") await b.click();
  await expect(b).toHaveAttribute("aria-pressed", "true", { timeout: 20_000 });
  await page.waitForTimeout(1200);
}

test("screenshots", async ({ browser }) => {
  test.setTimeout(900_000);
  mkdirSync(OUT, { recursive: true });
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: SIZES[0]!, colorScheme: "dark", deviceScaleFactor: 2 });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByTestId("open-video")).toBeVisible();
  await shoot(page, "home-empty");

  // A video straight from a file, once its words and waveform have arrived.
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  await expect.poll(() => page.getByTestId("transcript").locator(".w").count(), { timeout: 150_000 }).toBeGreaterThan(80);
  const tl = page.locator(".timeline-canvas");
  await expect.poll(async () => Number(await tl.getAttribute("data-audio-points")), { timeout: 120_000 }).toBeGreaterThan(0);
  await page.waitForTimeout(6000);
  await page.mouse.move(2, 400);
  await framing(page, "9:16");
  await shoot(page, "cut-9x16");
  await framing(page, "16:9");
  await shoot(page, "cut-16x9");
  await framing(page, "9:16");

  // An imported RT plan: overlays, gain envelopes and captions on the timeline.
  await page.getByRole("button", { name: "Home" }).first().click();
  await importPlanProject(page, "Shots", H79);
  await expect(page.getByTestId("cut-title")).toHaveText("My PC made this in 67 seconds", { timeout: 60_000 });
  await page.waitForTimeout(2500);
  // a selected part and the pointer over the timeline (hover line + time)
  const b = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const y = b.y + Number((await tl.getAttribute("data-lanes-y"))!.split(",")[1]) + 30;
  await page.mouse.click(b.x + 12 + 420 * ppf, y);
  await page.mouse.move(b.x + 12 + 900 * ppf, y);
  await shoot(page, "cut-plan");
  await page.mouse.move(2, 400);
  await page.keyboard.press("?");
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${OUT}/${LABEL}-keys-1440x900.png` });
  await page.keyboard.press("Escape");
  await page.keyboard.press("ControlOrMeta+k");
  await page.keyboard.type("hover");
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${OUT}/${LABEL}-palette-1440x900.png` });
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: "Home" }).first().click();
  await page.waitForTimeout(800);
  await shoot(page, "home");
});
