// Zoom sections on the lanes above the parts (decision 0046): the lanes turned on, a
// double-click adds a 2 s zoom there (selected, its controls in the inspector), its end
// edge drags later, the depth changes, ⌫ removes it, ⌘Z brings it back and ⌘⇧Z takes it
// away again (so the cut is left as it was found).
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("add, drag, edit, delete and undo a zoom section", async ({ browser }) => {
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

  // the lanes: off on a plain video until asked for
  const lanes = page.getByTestId("section-lanes");
  if (!(await lanes.count())) await page.getByTestId("section-lanes-toggle").click();
  await expect(lanes).toBeVisible();
  const bars = page.getByTestId("section-bar");
  const n0 = await bars.count();

  // a double-click on the empty zoom lane adds a 2 s zoom there, selected
  const body = page.getByTestId("section-lane-zoom").locator(".sl-body");
  const box = (await body.boundingBox())!;
  await body.dblclick({ position: { x: Math.round(box.width * 0.3), y: Math.round(box.height / 2) } });
  await expect(bars).toHaveCount(n0 + 1, { timeout: 20_000 });
  const bar = page.locator('[data-testid="section-bar"].sel');
  await expect(bar).toHaveCount(1);
  await expect(bar).toHaveAttribute("data-kind", "zoom");
  const id = (await bar.getAttribute("data-id"))!;
  const mine = page.locator(`[data-testid="section-bar"][data-id="${id}"]`);
  const start = Number(await mine.getAttribute("data-start-ms"));
  const end0 = Number(await mine.getAttribute("data-end-ms"));
  expect(end0 - start).toBeGreaterThan(1000);
  const controls = page.getByTestId("section-controls");
  await expect(controls).toBeVisible({ timeout: 10_000 });
  await expect(controls).toHaveAttribute("data-kind", "zoom");

  // its end edge dragged later (⌥: no snapping), one section_set
  const b = (await mine.boundingBox())!;
  await page.mouse.move(b.x + b.width - 2, b.y + b.height / 2);
  await page.mouse.down();
  await page.keyboard.down("Alt");
  for (let k = 1; k <= 8; k++) await page.mouse.move(b.x + b.width - 2 + k * 10, b.y + b.height / 2);
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect.poll(async () => Number(await mine.getAttribute("data-end-ms")), { timeout: 20_000 }).toBeGreaterThan(end0 + 200);
  expect(Number(await mine.getAttribute("data-start-ms"))).toBe(start);

  // the depth: 3×
  const depth = page.getByTestId("zoom-depth");
  await depth.evaluate((el: HTMLInputElement) => {
    el.value = "3";
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await expect(page.getByTestId("zoom-depth-value")).toHaveText("3×", { timeout: 20_000 });

  // ⌫ removes the selected section
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await mine.click();
  await expect(mine).toHaveClass(/sel/);
  await page.keyboard.press("Delete");
  await expect(bars).toHaveCount(n0, { timeout: 20_000 });

  // ⌘Z brings it back (with its new end and depth), ⌘⇧Z removes it again
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(bars).toHaveCount(n0 + 1, { timeout: 20_000 });
  await expect.poll(async () => Number(await mine.getAttribute("data-end-ms"))).toBeGreaterThan(end0 + 200);
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect(bars).toHaveCount(n0, { timeout: 20_000 });
  expect(errors).toEqual([]);
});
