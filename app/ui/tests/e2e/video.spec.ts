// Open any video (AJ 2026-10-14): paste a path (the native panel and drag-drop give the
// same call), land in the editor, words arrive in the transcript pane, click a word to
// seek, select words and Backspace cuts them (⌘Z puts them back), search finds words,
// and the video is listed under Recent projects.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

const seconds = async (page: import("@playwright/test").Page) => Number((await page.getByTestId("cut-meta").getAttribute("data-seconds")) ?? NaN);

test("open a video and cut its transcript", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByTestId("open-video")).toBeVisible();
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  // the side panel opens on Look (the words strip shows the words); the transcript is Words
  await expect(page.getByRole("tab", { name: "Look" })).toHaveAttribute("aria-selected", "true");
  await openTab(page, "Transcript");
  const d0 = await seconds(page);
  expect(d0).toBeGreaterThan(59);

  // Words arrive (the first window is 20 s, so some show before the whole video is heard).
  const words = page.getByTestId("transcript").locator(".w");
  await expect.poll(() => words.count(), { timeout: 120_000 }).toBeGreaterThan(20);
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  const total = await words.count();
  expect(total).toBeGreaterThan(80);

  // Click a word: the viewer seeks to it and the word is the playhead's.
  const w = words.nth(30);
  await w.click();
  await expect(w).toHaveClass(/now/, { timeout: 10_000 });
  await expect(page.getByTestId("transcript").locator(".w.now")).toHaveCount(1);

  // Select words 30..35 and Backspace: they leave the cut (greyed), the cut gets shorter.
  await words.nth(35).click({ modifiers: ["Shift"] });
  await page.keyboard.press("Backspace");
  await expect(words.nth(32)).toHaveClass(/out/, { timeout: 20_000 });
  await expect.poll(() => seconds(page)).toBeLessThan(d0 - 0.5);
  await expect.poll(async () => (await page.getByTestId("cut-meta").textContent()) ?? "").toContain("2 parts");
  // ⌘Z puts them back.
  await page.locator("body").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(words.nth(32)).not.toHaveClass(/out/, { timeout: 20_000 });
  await expect.poll(() => seconds(page)).toBeCloseTo(d0, 1);

  // The cut's checks are in its head.
  await expect(page.getByTestId("checks").locator("summary")).toBeVisible({ timeout: 20_000 });

  // Search.
  await page.getByLabel("Search the transcript").fill("banana");
  await expect(page.getByTestId("hits")).toHaveText(/^1 \/ [1-9]/);
  await expect(page.getByTestId("transcript").locator(".w.hit").first()).toBeVisible();
  // ⌘F replace, captions only: every "banana" shows as "Banana!" (audio untouched)
  await page.getByTestId("replace-with").fill("Banana!");
  await page.getByTestId("replace-all").click();
  await expect(page.getByText(/re-spelled in the captions/)).toBeVisible({ timeout: 20_000 });
  // ⌖ follow the playhead toggles
  await expect(page.getByTestId("follow")).toHaveAttribute("aria-pressed", "true");
  await page.getByTestId("follow").click();
  await expect(page.getByTestId("follow")).toHaveAttribute("aria-pressed", "false");
  // scrub audio is on by default and can be turned off
  await expect(page.getByTestId("scrub-audio")).toHaveAttribute("aria-pressed", "true");
  await page.getByTestId("scrub-audio").click();
  await expect(page.getByTestId("scrub-audio")).toHaveAttribute("aria-pressed", "false", { timeout: 10_000 });

  // Home lists its project under Recent projects.
  await page.getByRole("button", { name: "Home" }).click();
  // once (other specs in the same run may have opened other videos)
  await expect(page.getByRole("list", { name: "Recent projects" }).getByRole("listitem").filter({ hasText: "h79-v4" })).toHaveCount(1);
  expect(errors).toEqual([]);
});
