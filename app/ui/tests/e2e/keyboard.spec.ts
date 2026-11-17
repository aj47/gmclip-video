// Keyboard-only editing (lane K5): a whole edit with no mouse on AJ's h79 reel. J plays
// backwards (again faster), K stops; I and O mark a range and ⇧⌫ ripple-deletes it in one
// undo step; S splits at the playhead; V picks the edge nearest the playhead (again: the
// other side of the join) and ⌥→ / ⌥⇧← nudge it; ⌥↓ moves a part later; ⌥T puts the
// keys in the transcript, where ← → ↑ ↓ (⇧ extends) select words and D cuts them. The keys
// list (?) names every one of them.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bridge, open, projectNamed } from "./qa-lib";
import { labMedia } from "./lab";
import { transcribed } from "./analysed";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

interface CutV {
  frames: number;
  parts: { id: string; out: [number, number]; src: [number, number] }[];
}

const frameOf = async (page: Page) => Number((await page.getByTestId("timecode").getAttribute("data-frame")) ?? NaN);
const tl = (page: Page) => page.locator(".timeline-canvas");

test("a whole edit from the keyboard", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-kbd-")), "keyboard-test.mp4");
  copyFileSync(VIDEO, copy);
  const { page, errors } = await open(browser);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("keyboard-test", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  const pid = (await projectNamed(page, "keyboard-test"))!.id;
  const cutId = (await bridge<{ cut: string }>(page, "viewer_state", {})).cut;
  const view = () => bridge<CutV>(page, "cut_view", { cut: cutId }, pid);
  const v0 = await view();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });

  // ? names the new keys
  await page.keyboard.press("?");
  const dlg = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  for (const t of ["Play backwards", "Mark in / out", "Trim the part's in / out", "Ripple-delete the marked range", "Pick the part edge", "Nudge that edge", "Move the selected part", "In the transcript"]) await expect(dlg).toContainText(t);
  await page.keyboard.press("Escape");
  await expect(dlg).toHaveCount(0);

  // J from the end plays backwards; again ×1.5; K stops (forwards play is off the clock)
  await page.keyboard.press("End");
  await expect.poll(() => frameOf(page)).toBe(v0.frames - 1);
  await page.keyboard.press("j");
  await expect(page.getByTestId("rate")).toHaveText(/◀ ×1$/);
  await expect.poll(() => frameOf(page), { timeout: 5_000 }).toBeLessThan(v0.frames - 15);
  await page.keyboard.press("j");
  await expect(page.getByTestId("rate")).toHaveText(/◀ ×1\.5/);
  await page.keyboard.press("k");
  await expect(page.getByTestId("rate")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Play", exact: true })).toBeVisible();
  const stopped = await frameOf(page);
  await page.waitForTimeout(300);
  expect(await frameOf(page)).toBe(stopped);
  // L after J plays forwards at 1×; K stops
  await page.keyboard.press("j");
  await expect(page.getByTestId("rate")).toHaveText(/◀/);
  await page.keyboard.press("l");
  await expect(page.getByTestId("rate")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeVisible();
  await page.keyboard.press("k");
  await expect(page.getByRole("button", { name: "Play", exact: true })).toBeVisible();

  // I at 2 s, O at 4 s: the range; ⇧⌫ takes those 60 frames out (the part splits round
  // them) in one undo step
  await page.keyboard.press("Home");
  await expect.poll(() => frameOf(page)).toBe(0);
  for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect.poll(() => frameOf(page)).toBe(60);
  await page.keyboard.press("i");
  await expect(tl(page)).toHaveAttribute("data-mark-in", "60");
  for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect.poll(() => frameOf(page)).toBe(120);
  await page.keyboard.press("o");
  await expect(tl(page)).toHaveAttribute("data-range", "60-120");
  await page.keyboard.press("Shift+Backspace");
  await expect.poll(async () => (await view()).frames).toBe(v0.frames - 60);
  expect((await view()).parts.length).toBe(2);
  await expect(tl(page)).toHaveAttribute("data-range", "");
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await view()).frames).toBe(v0.frames);
  expect((await view()).parts.length).toBe(1);
  // Esc drops a half-marked range
  await page.keyboard.press("i");
  await expect(tl(page)).not.toHaveAttribute("data-mark-in", "");
  await page.keyboard.press("Escape");
  await expect(tl(page)).toHaveAttribute("data-mark-in", "");

  // S at 3 s (nothing hovered): two parts; V picks the join's right side (part 2's start),
  // V again its left (part 1's end); ⌥→ lengthens part 1 by a frame, ⌥⇧← shortens it by ten
  await page.keyboard.press("Home");
  await expect.poll(() => frameOf(page)).toBe(0);
  for (let i = 0; i < 9; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect.poll(() => frameOf(page)).toBe(90);
  await page.keyboard.press("s");
  await expect.poll(async () => (await view()).parts.length).toBe(2);
  await expect(page.getByTestId("cut-meta")).toContainText("2 parts");
  const [p1, p2] = (await view()).parts;
  await page.keyboard.press("v");
  await expect(tl(page)).toHaveAttribute("data-edge", `${p2!.id}:in`);
  await page.keyboard.press("v");
  await expect(tl(page)).toHaveAttribute("data-edge", `${p1!.id}:out`);
  // (parts are trims of one video, so part 1 may grow into frames part 2 plays too)
  await page.keyboard.press("Alt+ArrowRight");
  await expect.poll(async () => (await view()).parts[0]!.out[1]).toBe(91);
  await expect(tl(page)).toHaveAttribute("data-frames", String(v0.frames + 1));
  await page.keyboard.press("Alt+Shift+ArrowLeft");
  await expect.poll(async () => (await view()).parts[0]!.out[1]).toBe(81);
  await expect.poll(() => frameOf(page)).toBe(80);
  await page.keyboard.press("Escape");
  await expect(tl(page)).toHaveAttribute("data-edge", "");

  // ⌥↓ moves the part under the playhead (part 1) after part 2
  await page.keyboard.press("Home");
  await expect.poll(() => frameOf(page)).toBe(0);
  await page.keyboard.press("Alt+ArrowDown");
  await expect.poll(async () => (await view()).parts[1]!.id).toBe(p1!.id);
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await view()).parts[0]!.id).toBe(p1!.id);
  await openTab(page, "Transcript"); // the side panel opens on Look
  await expect(page.getByTestId("transcript").locator(".w").first()).toBeVisible();

  // ⌥T: the transcript takes the arrows; → selects the next word, ⇧→ extends, D cuts them
  await page.keyboard.press("Alt+KeyT");
  const tr = page.getByTestId("transcript");
  await expect(tr).toBeFocused();
  await page.keyboard.press("ArrowRight"); // the word at the playhead
  await page.keyboard.press("ArrowRight");
  const sel = tr.locator(".w.sel");
  await expect(sel).toHaveCount(1);
  const i0 = Number(await sel.first().getAttribute("data-i"));
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  await expect(sel).toHaveCount(3);
  await page.keyboard.press("ArrowDown");
  await expect(sel).toHaveCount(1);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowLeft");
  const before = (await view()).frames;
  await page.keyboard.press("Shift+ArrowRight");
  await expect(sel).toHaveCount(2);
  await page.keyboard.press("d");
  await expect.poll(async () => (await view()).frames, { timeout: 20_000 }).toBeLessThan(before);
  await expect(tr.locator(".w.out").first()).toBeVisible();
  expect(i0).toBeGreaterThanOrEqual(0);
  expect(errors).toEqual([]);
});
