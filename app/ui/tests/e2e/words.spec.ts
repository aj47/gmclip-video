// U.06: the words strip on a real reel cut (loc_4fadb756_h34, in its stream's project
// fixture): words in cut order by part, click seeks, drag selects, d strikes (the cut gets
// shorter and the words show struck), restore, * toggles a keyword. Skips without the lab media.
// U.06 remainder in the transcript pane, on AJ's h79 reel: F re-spells a word in captions
// (audio and the cut unchanged), [ ] { } tune a word's edges with a readout and ↺, and
// struck words can come back as a new part (＋ Add as part).
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { hasLab, labMedia, openLabProject } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!hasLab("preview"), "no lab media on this machine");

test("words strip on a real cut", async ({ browser }) => {
  // the words strip under the picture is retired (AJ 2026-11-22): the transcript is the Words
  // tab only (its Fix everywhere and B-roll moved there; see ideas.spec and transcript specs)
  test.skip(true, "the words strip is retired (2026-11-22)");
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await openLabProject(page, "loc_4fadb756");
  await page.getByRole("button", { name: "Home" }).click();
  const cutRow = page.locator('.project .item[data-framed="yes"]').filter({ hasText: "4 shots" }).first();
  await cutRow.getByRole("button", { name: "Open" }).click();
  await expect(page.getByTestId("cut-title")).toBeVisible();

  const strip = page.getByRole("listbox", { name: "Cut words" });
  await expect(strip).toBeVisible({ timeout: 30_000 });
  const live = strip.locator(".w:not(.struck)");
  const struck = strip.locator(".w.struck");
  expect(await live.count()).toBeGreaterThan(60);
  const struck0 = await struck.count();
  const head = page.getByTestId("cut-meta");
  const dur0 = await head.getAttribute("data-seconds");

  // Click seeks.
  const tc = page.getByTestId("timecode");
  const w20 = live.nth(20);
  await w20.click();
  await expect(tc).not.toHaveText("0:00.00 / " + (await tc.textContent())!.split(" / ")[1]);

  // Three consecutive words from the middle of the part with the most words.
  const byPart = await live.evaluateAll((els) => els.map((e) => Number((e as HTMLElement).dataset.part)));
  const counts = new Map<number, number>();
  byPart.forEach((p) => counts.set(p, (counts.get(p) ?? 0) + 1));
  const big = [...counts.entries()].sort((x, y) => y[1] - x[1])[0]![0];
  const first = byPart.indexOf(big) + Math.floor(counts.get(big)! / 2) - 1;
  const ords = await live.evaluateAll((els, k) => els.slice(k, k + 3).map((e) => (e as HTMLElement).dataset.i), first);
  // the strip scrolls: bring the words into its view before pointing at them
  await live.nth(first + 2).scrollIntoViewIfNeeded();
  await live.nth(first).scrollIntoViewIfNeeded();
  const a = (await live.nth(first).boundingBox())!;
  const b = (await live.nth(first + 2).boundingBox())!;
  await page.mouse.move(a.x + 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width - 2, b.y + b.height / 2, { steps: 6 });
  await page.mouse.up();
  await expect(page.getByRole("toolbar", { name: "Selection" })).toContainText("3 words");
  const live0 = await live.count();
  await page.keyboard.press("d");
  await expect.poll(() => struck.count(), { timeout: 20_000 }).toBe(struck0 + 3);
  expect(await live.count()).toBe(live0 - 3);
  await expect(head).not.toHaveAttribute("data-seconds", dur0!);
  for (const o of ords) await expect(strip.locator(`.w.struck[data-i="${o}"]`)).toHaveCount(1);

  // Restore: select the struck words and Restore.
  const s0 = strip.locator(`.w.struck[data-i="${ords[0]}"]`);
  const s2 = strip.locator(`.w.struck[data-i="${ords[2]}"]`);
  await s0.click();
  await s2.click({ modifiers: ["Shift"] });
  await page.getByRole("toolbar", { name: "Selection" }).getByRole("button", { name: "Restore" }).click();
  await expect.poll(() => struck.count(), { timeout: 20_000 }).toBe(struck0);

  // Keyword with *.
  await live.nth(40).click();
  await page.keyboard.press("*");
  await expect(strip.locator(".w.kw")).toHaveCount(1, { timeout: 20_000 });
  // Re-spell waits for the caption builder: the button says why.
  await live.nth(41).click();
  await expect(page.getByRole("button", { name: "✎ Re-spell" })).toBeDisabled();
  expect(errors).toEqual([]);
});

const nparts = async (page: import("@playwright/test").Page) => {
  const t = (await page.getByTestId("cut-meta").textContent()) ?? "";
  return Number(/(\d+) parts?/.exec(t)?.[1] ?? NaN);
};

test("re-spell, tune edges, add as part", async ({ browser }) => {
  test.skip(!existsSync(VIDEO), "no test video on this machine");
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
  await openTab(page, "Transcript"); // the side panel opens on Look
  const words = page.getByTestId("transcript").locator(".w");
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "h79-v4");

  // F re-spells (captions only).
  const w20 = words.nth(20);
  const heard = (await w20.textContent()) ?? "";
  await w20.hover();
  await page.keyboard.press("f");
  const box = page.getByLabel("Caption spelling");
  await expect(box).toBeVisible();
  await expect(box).toHaveValue(heard);
  await box.fill("BANANA");
  await box.press("Enter");
  await expect(w20).toHaveText("BANANA", { timeout: 10_000 });
  await expect(w20).toHaveClass(/respelled/);
  await expect(w20).toHaveAttribute("title", `heard “${heard}”`);

  // [ ] { } tune the edges; ↺ puts them back.
  const w25 = words.nth(25);
  await w25.hover();
  await page.keyboard.press("]");
  await page.keyboard.press("]");
  await page.keyboard.press("{");
  await expect(page.getByTestId("tune-readout")).toContainText("start +20 ms · end -10 ms", { timeout: 10_000 });
  await expect(w25).toHaveClass(/tuned/);
  await page.getByRole("button", { name: "↺ Edges" }).click();
  await expect(page.getByTestId("tune-readout")).toContainText("start 0 ms · end 0 ms", { timeout: 10_000 });
  await page.getByRole("button", { name: "Done tuning" }).click();

  // Cut words 40..45, then bring them back as a new part.
  const n0 = await nparts(page);
  await words.nth(40).click();
  await words.nth(45).click({ modifiers: ["Shift"] });
  await page.keyboard.press("Backspace");
  await expect(words.nth(42)).toHaveClass(/out/, { timeout: 20_000 });
  await expect.poll(() => nparts(page)).toBe(n0 + 1);
  await words.nth(40).click();
  await words.nth(45).click({ modifiers: ["Shift"] });
  await page.getByTestId("add-part").click();
  await expect.poll(() => nparts(page)).toBe(n0 + 2);
  await expect(words.nth(42)).not.toHaveClass(/out/, { timeout: 20_000 });
  expect(errors).toEqual([]);
});
