// The editor keys in the cut view (research/editor-inventory.md §1.15): help
// overlay, J/K/L shuttle, frame step, split, part jumps, ⇧I/⇧O, q/w, d/x on words, p, Delete.
// The keyboard-only edit (I/O marks, J backwards, V and ⌥-arrows) is keyboard.spec.ts.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

const head = async (page: Page) => (await page.getByTestId("cut-meta").textContent()) ?? "";
const seconds = async (page: Page) => Number((await page.getByTestId("cut-meta").getAttribute("data-seconds")) ?? NaN);
const nparts = async (page: Page) => Number(/(\d+) parts?/.exec(await head(page))?.[1] ?? NaN);
const frameOf = async (page: Page) => {
  const t = (await page.getByTestId("timecode").textContent()) ?? "";
  const m = /^(\d+):(\d+)\.(\d+)/.exec(t.trim());
  return m ? (Number(m[1]) * 60 + Number(m[2])) * 30 + Number(m[3]) : NaN;
};

test("editor keys", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  await openTab(page, "Transcript"); // the side panel opens on Look
  const words = page.getByTestId("transcript").locator(".w");
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }); // focus off the controls (its centre can be a button)

  // ? lists the keys; Esc closes.
  await page.keyboard.press("?");
  const dlg = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  await expect(dlg).toContainText("Split before the hovered word");
  await page.keyboard.press("Escape");
  await expect(dlg).toHaveCount(0);

  // → steps one frame; ⇧→ ten.
  const f0 = await frameOf(page);
  await page.keyboard.press("ArrowRight");
  await expect.poll(() => frameOf(page)).toBe(f0 + 1);
  await page.keyboard.press("Shift+ArrowRight");
  await expect.poll(() => frameOf(page)).toBe(f0 + 11);

  // L plays, L again ×1.5, K pauses at 1×.
  await page.keyboard.press("l");
  await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeVisible();
  await page.keyboard.press("l");
  await expect(page.getByTestId("rate")).toHaveText("×1.5");
  await page.keyboard.press("k");
  await expect(page.getByRole("button", { name: "Play", exact: true })).toBeVisible();
  await expect(page.getByTestId("rate")).toHaveCount(0);

  // S with a hovered word splits before it: 2 parts.
  await words.nth(30).hover();
  await page.keyboard.press("s");
  await expect.poll(() => nparts(page)).toBe(2);
  // ↑ / ↓ jump between part heads.
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  // calls over the bridge can overtake each other: be at 0 before stepping
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  await expect.poll(() => frameOf(page)).toBe(0);
  await page.keyboard.press("ArrowDown");
  await expect.poll(() => frameOf(page)).toBeGreaterThan(200);
  const head2 = await frameOf(page);
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => frameOf(page)).toBe(0);
  await page.keyboard.press("ArrowDown");
  await expect.poll(() => frameOf(page)).toBe(head2);

  // S with the pointer on the timeline splits at its hover line, not the playhead: 3 parts.
  const tl = page.locator(".timeline-canvas");
  const tb = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const scrolled = Number(await tl.getAttribute("data-scroll"));
  const partsY = tb.y + Number((await tl.getAttribute("data-lanes-y"))!.split(",")[1]) + 20;
  const at = head2 + 90;
  await page.mouse.move(tb.x + 12 + at * ppf - scrolled, partsY);
  await page.keyboard.press("s");
  await expect.poll(() => nparts(page)).toBe(3);
  const heads = (await page.getByRole("list", { name: "Parts" }).getByRole("button").allTextContents()).map((t) => Number(/frames (\d+)/.exec(t)![1]));
  expect(heads.some((h) => Math.abs(h - at) <= 2), `a part starts near ${at}: ${heads.join(",")}`).toBe(true);
  expect(await frameOf(page)).toBe(head2);

  // D on a hovered word cuts it; D again on it puts it back.
  const d0 = await seconds(page);
  await words.nth(40).hover();
  await page.keyboard.press("d");
  await expect(words.nth(40)).toHaveClass(/out/, { timeout: 20_000 });
  await expect.poll(() => seconds(page)).toBeLessThan(d0);
  await words.nth(40).hover();
  await page.keyboard.press("d");
  await expect(words.nth(40)).not.toHaveClass(/out/, { timeout: 20_000 });

  // Q on a hovered word trims the part's start to it (words before it leave the cut).
  await words.nth(50).hover();
  await page.keyboard.press("q");
  await expect(words.nth(45)).toHaveClass(/out/, { timeout: 20_000 });
  await expect(words.nth(50)).not.toHaveClass(/out/);
  await page.keyboard.press("ControlOrMeta+z");
  await expect(words.nth(45)).not.toHaveClass(/out/, { timeout: 20_000 });

  // ⇧O trims the part's out to the playhead (O alone marks the range's out).
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => frameOf(page)).toBe(0);
  for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
  const before = await seconds(page);
  await expect.poll(() => frameOf(page)).toBe(60);
  await page.keyboard.press("Shift+O");
  await expect.poll(() => seconds(page)).toBeLessThan(before - 1);
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => seconds(page)).toBeCloseTo(before, 1);

  // P plays exactly the hovered word, then stops.
  await words.nth(60).hover();
  const tcBefore = (await page.getByTestId("timecode").textContent()) ?? "";
  await page.keyboard.press("p");
  // it moved to the word, played it and stopped (bridge calls can overtake each other,
  // so wait for the move before the next key)
  await expect(page.getByTestId("timecode")).not.toHaveText(tcBefore);
  await expect(page.getByRole("button", { name: "Play", exact: true })).toBeVisible({ timeout: 5_000 });

  // ↓ selects the next part; Delete removes it; ⌘Z brings it back.
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  // calls over the bridge can overtake each other: be at 0 before stepping
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  await expect.poll(() => frameOf(page)).toBe(0);
  await page.keyboard.press("ArrowDown");
  await expect.poll(() => frameOf(page)).toBeGreaterThan(0);
  const n0 = await nparts(page);
  await page.keyboard.press("Delete");
  await expect.poll(() => nparts(page)).toBe(n0 - 1);
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => nparts(page)).toBe(n0);
  expect(errors).toEqual([]);
});
