// Ideas 2026-10-16, K.01 — "Draw your layout once" (SPEC §5.7.2) on a fresh video: the
// project map offers it for a landscape video with no layout, the sheet shows a frame
// ("Different frame" shows another), a screen, a camera and a private box are dragged
// on it, saved (one ⌘Z), and the map then names the layout; the Layout tab's callout is
// gone. Uses the lab media folder (tests/e2e/lab.ts).
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no lab media on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

/** Drags a box on the stage from (x0, y0) to (x1, y1), fractions of its size. */
async function drag(page: Page, x0: number, y0: number, x1: number, y1: number) {
  const st = (await page.getByTestId("dl-stage").boundingBox())!;
  await page.mouse.move(st.x + st.width * x0, st.y + st.height * y0);
  await page.mouse.down();
  await page.mouse.move(st.x + st.width * x1, st.y + st.height * y1, { steps: 5 });
  await page.mouse.up();
}

test("draw a video's layout once", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });

  // the map offers it: a landscape video with no layout
  const map = page.getByTestId("project-map");
  const row = map.getByTestId("map-source-layout");
  await expect(row).toHaveAttribute("data-layout", "");
  await row.getByTestId("draw-layout-open").click();
  const sheet = page.getByTestId("draw-layout");
  await expect(sheet).toBeVisible();
  await expect(sheet).toContainText("1920×1080");
  // the frame arrives (drawn on the canvas)
  await expect(sheet).toContainText("frame at", { timeout: 30_000 });
  const at0 = await sheet.getByTestId("dl-frame-at").textContent();
  await sheet.getByTestId("dl-frame").click();
  await expect.poll(() => sheet.getByTestId("dl-frame-at").textContent()).not.toBe(at0);

  // three boxes: screen (left), camera (bottom right), private (top right)
  const roles = sheet.getByTestId("dl-role");
  await roles.getByRole("button", { name: "Screen" }).click();
  await drag(page, 0.02, 0.02, 0.72, 0.95);
  await roles.getByRole("button", { name: "Camera" }).click();
  await drag(page, 0.76, 0.66, 0.98, 0.97);
  await roles.getByRole("button", { name: "Private" }).click();
  await drag(page, 0.76, 0.03, 0.98, 0.3);
  const boxes = sheet.getByTestId("dl-box");
  await expect(boxes).toHaveCount(3);
  // a second camera box replaces the first (one camera)
  await roles.getByRole("button", { name: "Camera" }).click();
  await drag(page, 0.75, 0.6, 0.99, 0.98);
  await expect(boxes).toHaveCount(3);
  await expect(sheet.locator('[data-testid="dl-box"][data-role="private"]')).toHaveCount(1);
  await sheet.getByTestId("dl-name").fill("Desk scene");
  await sheet.getByTestId("dl-save").click();
  await expect(sheet).toHaveCount(0);
  await expect(page.locator(".toast")).toContainText("Layout “Desk scene”");
  await expect(row).toHaveAttribute("data-layout", "Desk scene");

  // the video's stream layout holds the three boxes; one ⌘Z takes it away again
  const list = await bridge<{ id: string; name: string }[]>(page, "project_list_open", {});
  const pid = list[0]!.id;
  const v = await bridge<{ has_layout: boolean; regions: { role: string; rect: { x: number; w: number } }[] }>(
    page,
    "layout_draw_view",
    { source: (await bridge<{ sources: { id: string }[] }>(page, "project_map_view", {}, pid)).sources[0]!.id },
    pid,
  );
  expect(v.has_layout).toBe(true);
  expect(v.regions.map((r) => r.role).sort()).toEqual(["cam", "private", "screen"]);
  const cam = v.regions.find((r) => r.role === "cam")!;
  expect(cam.rect.x).toBeGreaterThan(0.7);
  expect(cam.rect.w).toBeGreaterThan(0.2);

  // reopen: the boxes are there, and the preset is offered
  await row.getByTestId("draw-layout-open").click();
  await expect(sheet.getByTestId("dl-box")).toHaveCount(3);
  await expect(sheet.getByTestId("dl-preset")).toHaveText("Desk scene");
  await sheet.getByRole("button", { name: "Skip" }).click();
  await expect(sheet).toHaveCount(0);

  // a phone-width window: the sheet fits without sideways scroll
  await row.getByTestId("draw-layout-open").click();
  await expect(sheet).toBeVisible();
  await page.setViewportSize({ width: 390, height: 800 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await page.keyboard.press("Escape");
  await expect(sheet).toHaveCount(0);
  expect(errors).toEqual([]);
});
