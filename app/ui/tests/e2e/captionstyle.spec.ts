// The caption style gallery (inspector): every style drawn on the current frame, a tile
// applies its style (one undo step), and "Save as my style" keeps the look in my styles.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("caption style gallery: preview, apply, undo, save as my style", async ({ browser }) => {
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
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Short");
  await page.getByTestId("captions-on").check();
  await expect.poll(async () => Number(((await page.getByTestId("caption-count").textContent()) ?? "0").split(" ")[0]), { timeout: 20_000 }).toBeGreaterThan(40);

  // The gallery: every built-in drawn on the frame, Story current.
  await openTab(page, "Inspector");
  await page.getByTestId("gallery-toggle").click();
  const tiles = page.getByTestId("style-tile");
  await expect(tiles.first().locator("canvas")).toBeVisible({ timeout: 60_000 });
  expect(await tiles.count()).toBeGreaterThanOrEqual(6);
  await expect(page.locator('[data-name="story"].current')).toHaveCount(1);
  // the thumbnails are drawn pictures, not blanks: two styles differ
  const pixels = async (name: string) =>
    page.locator(`[data-name="${name}"] canvas`).evaluate((c: HTMLCanvasElement) => {
      const d = c.getContext("2d")!.getImageData(0, 0, c.width, c.height).data;
      let h = 0;
      for (let i = 0; i < d.length; i += 4) h = (h * 31 + d[i]! + d[i + 1]! * 3 + d[i + 2]! * 7) >>> 0;
      return h;
    });
  expect(await pixels("story")).not.toEqual(await pixels("phrase_box"));
  if (process.env.GMCLIP_SHOT) await page.getByTestId("caption-gallery").screenshot({ path: process.env.GMCLIP_SHOT });

  // Apply word bounce: it becomes current; undo puts Story back.
  await page.locator('[data-name="word_bounce"] .look-pick').click();
  await expect(page.locator('[data-name="word_bounce"].current')).toHaveCount(1, { timeout: 30_000 });
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.locator('[data-name="story"].current')).toHaveCount(1, { timeout: 30_000 });

  // A free-font style: Figtree is added to the project on apply.
  await page.locator('[data-name="bold_pop"] .look-pick').click();
  await expect(page.locator('[data-name="bold_pop"].current')).toHaveCount(1, { timeout: 30_000 });

  // Save as my style: it joins the gallery as the project's and is current.
  await page.getByTestId("style-name").fill("AJ pop");
  await page.getByTestId("style-save").click();
  await expect(page.locator('[data-name="AJ pop"]')).toHaveCount(1, { timeout: 30_000 });
  await expect(page.locator('[data-name="AJ pop"] canvas')).toBeVisible({ timeout: 60_000 });
  // Fine control: the framing's animation, keyword pop and grouping.
  // (data-value is the look as read back; a select's own value is whatever was picked)
  // Two changes at once: the second builds on the first (it used to send the look as last
  // read, putting the animation back).
  await page.getByTestId("cap-anim").selectOption("fade");
  await page.getByTestId("cap-kw").selectOption("pop");
  await expect(page.getByTestId("cap-kw")).toHaveAttribute("data-value", "pop", { timeout: 20_000 });
  await expect(page.getByTestId("cap-anim")).toHaveAttribute("data-value", "fade", { timeout: 20_000 });
  await page.getByTestId("cap-grouping").selectOption("readable_phrase");
  await expect(page.getByTestId("cap-grouping")).toHaveAttribute("data-value", "readable_phrase", { timeout: 20_000 });

  // The captions as a subtitle file.
  await page.getByTestId("captions-srt").click();
  await expect(page.getByTestId("captions-written")).toContainText(/\d+ cues · .*\.srt/, { timeout: 30_000 });
  expect(errors).toEqual([]);
});
