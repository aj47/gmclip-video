// U.09 in the app: RT captions on, a correction, a text hook, a section pill and a
// measured run clock on an opened 16:9 talk shown as 9:16; export starts.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("short core: captions, hook, pill, run clock", async ({ browser }) => {
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

  // Captions on: cues built from the transcript.
  await page.getByTestId("captions-on").check();
  await expect.poll(async () => Number(((await page.getByTestId("caption-count").textContent()) ?? "0").split(" ")[0]), { timeout: 20_000 }).toBeGreaterThan(40);

  // The cue tool: split a cue at its second word, then join it back.
  await page.getByTestId("cue-list").locator("summary").click();
  const row = page.getByTestId("cue-row").filter({ has: page.locator(".cue-word:nth-child(2)") }).first();
  const second = row.locator(".cue-word").nth(1);
  const word = (await second.textContent()) ?? "";
  await second.click();
  // the word now opens a cue of its own
  await expect(page.locator(".cue-word.hand-split").first()).toHaveText(word, { timeout: 20_000 });
  await expect(page.locator(".cue-word:first-child.hand-split")).toHaveCount(1);
  await expect(page.getByTestId("cue-list")).toContainText("1 set by hand");
  await page.locator(".cue-word.hand-split").first().click();
  await expect(page.getByTestId("cue-list")).not.toContainText("set by hand");

  // The keywords list stars every match across the cut.
  const words = page.locator(".cue-word");
  const target = ((await words.nth(2).textContent()) ?? "").replace(/[^A-Za-z0-9']/g, "");
  await page.getByTestId("keywords").fill(target);
  await page.getByTestId("keywords-star").click();
  await expect(page.locator(".cue-word.star").first()).toBeVisible({ timeout: 20_000 });
  const starred = await page.locator(".cue-word.star").allTextContents();
  expect(starred.every((t) => t.replace(/[^A-Za-z0-9']/g, "").toLowerCase() === target.toLowerCase())).toBe(true);
  await expect(page.getByTestId("keywords")).toHaveValue(target);
  await page.getByTestId("keywords").fill("");
  await page.getByTestId("keywords-star").click();
  await expect(page.locator(".cue-word.star")).toHaveCount(0, { timeout: 20_000 });

  // Keep words together, and the caption height (behind Advanced, with names and corrections).
  await page.getByTestId("captions-more").locator("summary").click();
  await page.getByTestId("keep").fill("my local");
  await page.getByTestId("keep-add").click();
  await expect(page.getByTestId("keep-list")).toContainText("my local", { timeout: 20_000 });
  await page.getByTestId("caption-y").fill("1110");
  await page.getByRole("button", { name: "Move" }).click();
  await expect(page.getByTestId("caption-y")).toHaveAttribute("placeholder", "1110", { timeout: 20_000 });

  // A term: the gate lists "VM" (shown) against "Vm" (the term), Fix re-spells it.
  await page.getByTestId("term-written").fill("Vm");
  await page.getByTestId("term-heard").fill("V M");
  await page.getByTestId("term-add").click();
  await expect(page.getByTestId("name-row").first()).toContainText("→ Vm", { timeout: 20_000 });
  const rows = await page.getByTestId("name-row").count();
  await page.getByTestId("name-row").first().getByRole("button", { name: "Spell it Vm" }).click();
  await expect(page.getByTestId("name-row")).toHaveCount(rows - 1, { timeout: 20_000 });

  // A correction.
  await page.getByLabel("Heard as", { exact: true }).fill("VM");
  await page.getByLabel("Show as").fill("V.M.");
  await page.getByRole("button", { name: "Fix", exact: true }).click();
  await expect(page.locator(".fix")).toContainText("V.M.");

  // The hook for 3 s.
  await page.getByLabel("Hook text").fill("Every project gets its own VM");
  await page.getByLabel("Hook seconds").fill("3");
  await page.getByRole("button", { name: "Set hook" }).click();
  await expect(page.locator('.overlay-row[data-role="text_hook"]')).toContainText("Every project gets its own VM");

  // A pill on the part at the playhead.
  await page.getByLabel("Pill text").fill("MY DEV VM SETUP");
  await page.getByRole("button", { name: "Add to this part" }).click();
  await expect(page.locator('.overlay-row[data-role="section_pill"]')).toContainText("MY DEV VM SETUP");

  // Mark a run from 4 s to 12 s and make the clock.
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  // calls over the bridge can overtake each other: be at 0 before stepping
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  for (let i = 0; i < 12; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:04\.00/);
  await page.getByTestId("run-start").click();
  for (let i = 0; i < 24; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:12\.00/);
  await page.getByTestId("run-end").click();
  await page.getByTestId("clock-more").locator("summary").click();
  await page.getByLabel("Running label").fill("BOOTING");
  await page.getByTestId("make-clock").click();
  await expect(page.locator('.overlay-row[data-role="run_clock"]')).toContainText("BOOTING → DONE IN");

  // A run measured in the stream viewer: browse at 2 s, mark, step 3 s, mark, measure.
  await page.getByTestId("stream-at").fill("2");
  await page.getByTestId("stream-open").click();
  await expect(page.getByTestId("stream-bar")).toBeVisible({ timeout: 20_000 });
  await expect.poll(async () => Number(await page.getByTestId("stream-now").textContent()), { timeout: 20_000 }).toBeCloseTo(2, 1);
  await page.getByTestId("stream-mark-start").click();
  await page.locator(".cut-head").hover();
  for (let i = 0; i < 9; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect.poll(async () => Number(await page.getByTestId("stream-now").textContent()), { timeout: 20_000 }).toBeCloseTo(5, 1);
  await page.getByTestId("stream-mark-end").click();
  await page.getByTestId("stream-measure").click();
  await expect(page.getByText(/Measured 3\.0 s/)).toBeVisible({ timeout: 20_000 });
  await page.getByTestId("stream-back").click();
  await expect(page.getByTestId("stream-bar")).toHaveCount(0);

  // Hold the take's screen on a stream frame (RT's hold_at), in the inspector.
  await openTab(page, "Inspector");
  if (!(await page.getByTestId("hold-at").isVisible())) await page.getByText("Trim by frames and hold").click();
  await page.getByTestId("hold-at").fill("1.5");
  await page.getByRole("button", { name: "Hold", exact: true }).click();
  await expect(page.getByTestId("hold-at")).toHaveValue("", { timeout: 20_000 });
  await openTab(page, "Short");

  // Export: the term gate stops it first ("V.M." is not "Vm"); export as is.
  await page.getByTestId("export").click();
  await expect(page.getByTestId("term-gate")).toContainText("→ Vm", { timeout: 20_000 });
  await page.getByTestId("term-gate-ignore").click();
  await expect(page.getByTestId("export-progress")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Cancel" }).click();
  expect(errors).toEqual([]);
});
