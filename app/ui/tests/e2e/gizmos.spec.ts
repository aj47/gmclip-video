// U.12 (R-GEST-1, SPEC §7 gizmos) in the app: on the paused frame the text hook is a box
// on its glyphs that drags to move (one transaction, one ⌘Z), Esc mid-drag puts it back
// with nothing written, and the caption seat drags up or down (one transaction). While
// either drags, the frame itself shows the move (the render thread draws it: the viewer
// reports the live move and renders new frames) and after the release no live move is
// left. The core side (agent writes refused during each drag, the preview's pixels equal
// the committed plan's) is `tests/gesture.rs`.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openPage, VIDEO } from "./ux-lib";
import { openTab } from "./tabs";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;
async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

test("text hook and caption seat drags are one transaction each; Esc cancels", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-gz-")), "gizmo-test.mp4");
  copyFileSync(VIDEO, copy);
  const { page, errors } = await openPage(browser, { width: 1100, height: 700 });
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("gizmo-test", { timeout: 30_000 });
  const open = await bridge<{ id: string; name: string }[]>(page, "project_list_open", {}, "");
  const pid = open.find((p) => p.name.includes("gizmo-test"))?.id ?? "";
  const rev = async () => (await bridge<{ rev: number }>(page, "project_save_state", {}, pid)).rev;
  type Vs = { text_drag?: [number, number] | null; rendered: number };
  const vs = async () => await bridge<Vs>(page, "viewer_state", {}, pid);

  await openTab(page, "Short");
  await page.getByLabel("Hook text").fill("My PC made this");
  await page.getByLabel("Hook seconds").fill("3");
  await page.getByRole("button", { name: "Set hook" }).click();
  const hook = page.getByTestId("gizmo-text");
  await expect(hook).toHaveCount(1, { timeout: 20_000 });
  await expect(hook).toHaveAttribute("data-role", "text_hook");
  const b0 = (await hook.boundingBox())!;
  const c = (await page.locator(".viewer-canvas").boundingBox())!;
  // the box sits in the top band of the frame, on a line of text
  expect(b0.y).toBeGreaterThan(c.y);
  expect(b0.y + b0.height).toBeLessThan(c.y + c.height * 0.3);
  expect(b0.width).toBeGreaterThan(c.width * 0.3);

  // Esc mid-drag: back where it was, nothing written
  const r0 = await rev();
  await page.mouse.move(b0.x + b0.width / 2, b0.y + b0.height / 2);
  await page.mouse.down();
  await page.mouse.move(b0.x + b0.width / 2, b0.y + b0.height / 2 + 60, { steps: 6 });
  await expect(hook).toHaveClass(/dragging/);
  // the frame previews the move: the render thread draws the hook 60 screen px down
  const scale = (await page.evaluate(() => {
    const cv = document.querySelector<HTMLCanvasElement>(".viewer-canvas");
    return cv ? cv.getBoundingClientRect().width : 0;
  })) / 1080;
  await expect.poll(async () => (await vs()).text_drag?.[1] ?? 0, { timeout: 10_000 }).toBeGreaterThan(57 / scale);
  expect((await vs()).text_drag?.[1] ?? 0).toBeLessThan(63 / scale);
  const n0 = (await vs()).rendered;
  await page.mouse.move(b0.x + b0.width / 2, b0.y + b0.height / 2 + 80, { steps: 4 });
  await expect.poll(async () => (await vs()).rendered, { timeout: 10_000 }).toBeGreaterThan(n0);
  await page.keyboard.press("Escape");
  await page.mouse.up();
  await expect(hook).not.toHaveClass(/dragging/);
  // Esc: the frame goes back at once
  await expect.poll(async () => (await vs()).text_drag ?? null, { timeout: 10_000 }).toBeNull();
  expect((await hook.boundingBox())!.y).toBeCloseTo(b0.y, 0);
  expect(await rev()).toBe(r0);

  // a drag of 120 moves: one transaction; the box follows the glyphs to their new place
  await page.mouse.move(b0.x + b0.width / 2, b0.y + b0.height / 2);
  await page.mouse.down();
  for (let i = 1; i <= 120; i++) await page.mouse.move(b0.x + b0.width / 2 + (i % 2), b0.y + b0.height / 2 + i * 0.5);
  // mid-drag the frame shows it (half a screen px down per move: 60 px)
  await expect.poll(async () => (await vs()).text_drag?.[1] ?? 0, { timeout: 10_000 }).toBeGreaterThan(40 / scale);
  await page.mouse.up();
  await expect.poll(rev, { timeout: 20_000 }).toBe(r0 + 1);
  await expect(hook).not.toHaveClass(/dragging/);
  // the re-planned cut replaces the preview: no live move is left (R-COMP-1 holds)
  await expect.poll(async () => (await vs()).text_drag ?? null, { timeout: 10_000 }).toBeNull();
  await expect.poll(async () => (await hook.boundingBox())!.y, { timeout: 20_000 }).toBeGreaterThan(b0.y + 40);
  // its centre snapped onto the frame's centre line
  const b1 = (await hook.boundingBox())!;
  expect(Math.abs(b1.x + b1.width / 2 - (c.x + c.width / 2))).toBeLessThan(3);
  // ⌘Z: one step back
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await hook.boundingBox())!.y, { timeout: 20_000 }).toBeCloseTo(b0.y, 0);

  // the caption seat: captions on (heard), the seat handle drags the cues down
  await page.getByTestId("captions-on").check();
  const seat = page.getByTestId("gizmo-seat");
  await expect(seat).toHaveCount(1, { timeout: 180_000 });
  const y0 = Number(await seat.getAttribute("data-y"));
  const s0 = (await seat.boundingBox())!;
  const r1 = await rev();
  await page.mouse.move(s0.x + 12, s0.y + s0.height / 2);
  await page.mouse.down();
  await page.mouse.move(s0.x + 12, s0.y + s0.height / 2 + 30, { steps: 8 });
  // the captions move in the frame while the seat drags
  await expect.poll(async () => (await vs()).text_drag?.[1] ?? 0, { timeout: 10_000 }).toBeGreaterThan(20 / scale);
  await page.mouse.up();
  await expect.poll(async () => Number(await seat.getAttribute("data-y")), { timeout: 20_000 }).toBeGreaterThan(y0 + 40);
  await expect.poll(rev, { timeout: 20_000 }).toBe(r1 + 1);
  await expect.poll(async () => (await vs()).text_drag ?? null, { timeout: 10_000 }).toBeNull();
  expect(errors).toEqual([]);
  await page.close();
});
