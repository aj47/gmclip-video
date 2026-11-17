// Vertical crops on an opened 16:9 video: the output toggle, a Screen + facecam preset,
// crop boxes on the source (move, resize keeping the shape, Esc cancels a drag), apply to all parts, back to
// 16:9, and export still starts.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

const ratio = async (page: Page) => {
  const b = (await page.locator(".viewer-canvas").boundingBox())!;
  return b.width / b.height;
};
const box = async (page: Page, role: string) => (await page.locator(`.crop-box[data-role=${role}]`).boundingBox())!;

test("vertical crops", async ({ browser }) => {
  test.setTimeout(240_000);
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
  const out = page.getByRole("group", { name: "Output" });
  await expect(out.getByRole("button", { name: "16:9" })).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => ratio(page)).toBeCloseTo(16 / 9, 1);

  // 9:16.
  await out.getByRole("button", { name: "9:16" }).click();
  await expect(out.getByRole("button", { name: "9:16" })).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => ratio(page)).toBeCloseTo(9 / 16, 1);

  // Screen + facecam on the part.
  await openTab(page, "Layout");
  await page.getByRole("button", { name: "Screen + facecam" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");

  // Crop on the source: two boxes over a 16:9 picture.
  await page.getByTestId("crop-mode").click();
  await expect.poll(() => ratio(page)).toBeCloseTo(16 / 9, 1);
  await expect(page.locator(".crop-box")).toHaveCount(2);
  // Move the facecam box right by 60 px.
  const c0 = await box(page, "cam");
  await page.mouse.move(c0.x + c0.width / 2, c0.y + c0.height / 2);
  await page.mouse.down();
  await page.mouse.move(c0.x + c0.width / 2 + 30, c0.y + c0.height / 2, { steps: 4 });
  await page.mouse.move(c0.x + c0.width / 2 + 60, c0.y + c0.height / 2, { steps: 4 });
  await page.mouse.up();
  await expect.poll(async () => (await box(page, "cam")).x, { timeout: 20_000 }).toBeGreaterThan(c0.x + 50);
  // U.12: Esc mid-drag puts the box back and writes nothing (the drag is a held gesture)
  const c1 = await box(page, "cam");
  await page.mouse.move(c1.x + c1.width / 2, c1.y + c1.height / 2);
  await page.mouse.down();
  await page.mouse.move(c1.x + c1.width / 2 - 50, c1.y + c1.height / 2 + 20, { steps: 5 });
  expect((await box(page, "cam")).x).toBeLessThan(c1.x - 30);
  await page.keyboard.press("Escape");
  await page.mouse.up();
  expect((await box(page, "cam")).x).toBeCloseTo(c1.x, 0);
  await page.waitForTimeout(500);
  expect((await box(page, "cam")).x).toBeCloseTo(c1.x, 0);
  // Resize the screen box from its bottom-right corner: smaller, same shape.
  const s0 = await box(page, "screen");
  const g = (await page.locator(".crop-box[data-role=screen] .crop-grip.g3").boundingBox())!;
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await page.mouse.down();
  await page.mouse.move(g.x - 40, g.y - 10, { steps: 5 });
  await page.mouse.move(g.x - 80, g.y - 20, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => (await box(page, "screen")).width, { timeout: 20_000 }).toBeLessThan(s0.width - 60);
  const s1 = await box(page, "screen");
  expect(s1.width / s1.height).toBeCloseTo(s0.width / s0.height, 1);
  expect(s1.x).toBeCloseTo(s0.x, 0);
  // ⌘Z undoes the resize.
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }); // focus off the controls (its centre can be a button)
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await box(page, "screen")).width, { timeout: 20_000 }).toBeCloseTo(s0.width, 0);
  await page.getByTestId("crop-mode").click();
  await expect.poll(() => ratio(page)).toBeCloseTo(9 / 16, 1);

  // Split: both halves keep the layout; give part 2 Full screen, then part 1's to all.
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  // calls over the bridge can overtake each other: be at 0 before stepping
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:01\.20/);
  await page.keyboard.press("s");
  await expect.poll(async () => (await page.getByTestId("cut-meta").textContent()) ?? "").toContain("2 parts");
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");
  await page.getByRole("button", { name: "Full screen" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Full screen 9:16");
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ArrowUp");
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");
  await page.getByTestId("apply-all").click();
  // down to part 2 (its head, 0:01.20): both parts now read the same layout, so wait on
  // the timecode; a press before the viewer has reported the seek up is lost (the jump
  // reads the viewer's frame), seen on Windows; part 2 is the last, so a repeat stays there
  await expect(async () => {
    await page.locator(".cut-head").hover();
    await page.keyboard.press("ArrowDown");
    await expect(page.getByTestId("timecode")).toHaveText(/^0:01\.20/, { timeout: 2_000 });
  }).toPass({ timeout: 20_000 });
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");

  // By numbers: a detail inset on part 2, its crop typed, saved as a preset, used on part 1.
  const insp = page.getByRole("complementary", { name: "Inspector" });
  await page.getByTestId("inset-add").click();
  await expect(page.getByText("Detail inset added")).toBeVisible({ timeout: 20_000 });
  const tab = (name: string) => openTab(page, name);
  await tab("Inspector");
  const detail = insp.getByRole("region", { name: "Layer detail" });
  await expect(detail).toHaveCount(1, { timeout: 20_000 });
  const cx = detail.getByRole("group", { name: "Crop (source pixels)" }).getByLabel("x");
  await cx.fill("200");
  await cx.press("Enter");
  await expect(cx).toHaveValue("200", { timeout: 20_000 });
  await tab("Layout");
  await page.getByTestId("preset-name").fill("screen + inset");
  await page.getByTestId("preset-save").click();
  await expect(page.getByTestId("preset-pick")).toHaveValue("screen + inset");
  // up to part 1 (as above, a press can be lost); part 1 is the first, so a repeat stays there
  await expect(async () => {
    await page.locator(".cut-head").hover();
    await page.keyboard.press("ArrowUp");
    await tab("Inspector");
    await expect(insp.getByRole("region", { name: "Layer detail" })).toHaveCount(0, { timeout: 2_000 });
  }).toPass({ timeout: 20_000 });
  await tab("Layout");
  await page.getByTestId("preset-apply").click();
  await tab("Inspector");
  await expect(insp.getByRole("region", { name: "Layer detail" })).toHaveCount(1, { timeout: 20_000 });
  await expect(insp.getByRole("region", { name: "Layer detail" }).getByRole("group", { name: "Crop (source pixels)" }).getByLabel("x")).toHaveValue("200");
  // remove it again, then copy part 1's layout (no inset) to the next part
  await insp.getByTestId("inset-remove").click();
  await expect(insp.getByRole("region", { name: "Layer detail" })).toHaveCount(0, { timeout: 20_000 });
  await tab("Layout");
  await page.getByTestId("copy-next").click();
  await expect(page.getByText("layout is on the next 1 part")).toBeVisible({ timeout: 20_000 });
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ArrowDown");
  await tab("Inspector");
  await expect(insp.getByRole("region", { name: "Layer detail" })).toHaveCount(0, { timeout: 20_000 });

  await tab("Layout");

  // Back to 16:9 and to 9:16: the work is kept.
  await out.getByRole("button", { name: "16:9" }).click();
  await expect.poll(() => ratio(page)).toBeCloseTo(16 / 9, 1);
  await out.getByRole("button", { name: "9:16" }).click();
  await expect.poll(() => ratio(page)).toBeCloseTo(9 / 16, 1);

  // Export starts on the 9:16 cut.
  await page.getByTestId("export").click();
  await expect(page.getByTestId("export-progress")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Cancel" }).click();
  expect(errors).toEqual([]);
});
