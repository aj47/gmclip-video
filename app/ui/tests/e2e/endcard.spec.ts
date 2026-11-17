// End card to the long-form (lane K5) on AJ's h79 reel: once the stream has a long-form that
// plays this short's moment, the Short tab suggests "Full video:" + its title, a frame and an
// arrow over the last 3 s; one click adds it (with Undo in the toast), ⌘Z takes it off, and
// Remove does too. Nothing is added until the click.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bridge, open, projectNamed } from "./qa-lib";
import { labMedia } from "./lab";
import { transcribed } from "./analysed";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
const SEC = 705_600_000;
const SHOTS = process.env.GMCLIP_SHOTS_DIR;

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("a short is offered an end card to its long-form", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-endcard-")), "endcard-test.mp4");
  copyFileSync(VIDEO, copy);
  const { page, errors } = await open(browser);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("endcard-test", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  const pid = (await projectNamed(page, "endcard-test"))!.id;
  const stream = (await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid)).cut;
  // a short from a moment of the stream (5 s – 20 s, on sentence edges)
  const m = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 150, out_frame: 600 }, pid);
  const cut = (await bridge<{ cut: string }>(page, "cut_from_moment", { moment: m.moment.id }, pid)).cut;
  await page.getByTestId("map-short").first().locator(".map-open").click();
  await expect.poll(async () => (await bridge<{ cut: string }>(page, "viewer_state", {})).cut).toBe(cut);
  await openTab(page, "Short");
  const box = page.getByTestId("endcard-lf");
  // no long-form yet: it says why, and offers nothing
  await expect(box.getByTestId("endcard-lf-why")).toContainText("No long-form");
  await expect(box.getByTestId("endcard-lf-add")).toHaveCount(0);

  // a long-form of the stream over the same seconds (topics → long-form)
  const w = (n: number) => ({ word_id: { asr_revision: "r1", ordinal: n }, t_src_start: n * SEC, t_src_end: n * SEC + SEC / 2, normalised_text: "w" });
  const t = await bridge<{ topic: string }>(page, "topic_set", { start: w(5), end: w(40), title: "The banana" }, pid);
  await bridge(page, "longform_from_topics", { topics: [t.topic], title: "The talking banana, start to finish" }, pid);
  // the panel reads it again when the cut changes; nudge it by switching tabs
  await openTab(page, "Transcript");
  await openTab(page, "Short");
  await expect(box).toHaveAttribute("data-available", "1");
  await expect(box).toContainText("The talking banana, start to finish");
  await expect(box.locator(".lf-card-preview")).toContainText("Full video:");
  if (SHOTS) await box.screenshot({ path: join(SHOTS, "endcard-suggest.png") });
  const view = () => bridge<{ has_card: boolean }>(page, "end_card_longform_view", { cut }, pid);
  expect((await view()).has_card).toBe(false);

  // one click adds it; the toast's Undo (⌘Z) takes it off
  await box.getByTestId("endcard-lf-add").click();
  await expect(box.getByTestId("endcard-lf-on")).toContainText("Full video: The talking banana");
  expect((await view()).has_card).toBe(true);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await view()).has_card).toBe(false);
  await expect(box.getByTestId("endcard-lf-add")).toBeVisible();
  // again, then Remove
  await box.getByTestId("endcard-lf-add").click();
  await expect(box.getByTestId("endcard-lf-remove")).toBeVisible();
  if (SHOTS) await box.screenshot({ path: join(SHOTS, "endcard-on.png") });
  await box.getByTestId("endcard-lf-remove").click();
  await expect.poll(async () => (await view()).has_card).toBe(false);
  expect(errors).toEqual([]);
});
