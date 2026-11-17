// Several videos in one project (AJ 2026-10-15: "i should be able to load multiple video
// files into one project"): three small videos dropped on Home open as ONE project (the
// sheet asks "Are these the same recording?" for the two of about the same length —
// no, here); the map lists three videos, each transcribed in turn, all on the one main
// cut (decision 0036); a topic on two of its videos (picked in the transcript's source
// picker) makes a long-form across them, the third video's words are added
// to it from the transcript's source picker, and the mixed cut exports; removing a video
// the long-form plays is refused, naming it. The videos are cut here with ffmpeg (a dev
// tool) from AJ's h79 reels.
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const V4 = labMedia("h79/h79-v4.mp4");
const V3 = labMedia("h79/h79-v3.mp4");
const FF = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].find((p) => existsSync(p));

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(V4) || !existsSync(V3) || !FF, "no test videos or ffmpeg on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge(page: Page, command: string, args: Record<string, unknown>): Promise<void> {
  const r = await page.evaluate(
    ([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a),
    [command, args] as [string, Record<string, unknown>],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
}

function cut(src: string, from: number, dur: number, out: string) {
  execFileSync(FF!, ["-v", "error", "-y", "-ss", String(from), "-t", String(dur), "-i", src, "-vf", "scale=360:640", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "30", "-r", "30", "-c:a", "aac", "-ar", "48000", "-ac", "2", out]);
}

test("three videos, one project, a cut across them", async ({ browser }) => {
  test.setTimeout(420_000);
  const dir = mkdtempSync(join(tmpdir(), "gmclip-multi-"));
  const [a, b, c] = ["alpha.mp4", "bravo.mp4", "charlie.mp4"].map((f) => join(dir, f)) as [string, string, string];
  cut(V4, 0, 8, a);
  cut(V4, 20, 8.4, b);
  cut(V3, 2, 12, c);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByTestId("open-video")).toBeVisible();
  // the page has subscribed to Home's events (a drop before that is heard by no one)
  await expect(page.locator("html")).toHaveAttribute("data-booted", "1", { timeout: 60_000 });

  // Drop three files on Home (what a native drop sends).
  await bridge(page, "files_dropped", { paths: [a, b, c] });
  const sheet = page.getByRole("dialog", { name: "Import videos" });
  await expect(sheet).toBeVisible({ timeout: 60_000 });
  await expect(sheet.getByTestId("import-file")).toHaveCount(3);
  // alpha (8 s) and bravo (8.4 s) agree within 10 %: asked, yes by default; no here
  const same = sheet.getByTestId("same-0-1");
  await expect(same).toContainText("Are these the same recording?");
  await expect(same.getByRole("button", { name: "Yes, one video" })).toHaveAttribute("aria-pressed", "true");
  await same.getByRole("button", { name: "No, separate videos" }).click();
  await expect(sheet.getByTestId("one-project").getByRole("button", { name: "One project with these 3 videos" })).toHaveAttribute("aria-pressed", "true");
  await sheet.getByTestId("import-many-go").click();
  await expect(page.getByTestId("cut-title")).toHaveText("alpha", { timeout: 60_000 });

  // The map: three videos, each transcribed (one at a time), all on the main cut.
  const map = page.getByTestId("project-map");
  const rows = map.getByTestId("map-source");
  await expect(rows).toHaveCount(3, { timeout: 20_000 });
  await expect(rows.nth(1)).toHaveAttribute("data-name", "bravo");
  await expect(rows.nth(2)).toContainText("0:12");
  for (let i = 0; i < 3; i++) await expect(rows.nth(i)).toHaveAttribute("data-asr", "ok", { timeout: 300_000 });
  await expect(rows.first().getByTestId("map-source-asr")).toHaveText("transcribed");
  await expect(map.getByTestId("map-stream")).toHaveCount(1);
  await expect(map.getByTestId("map-stream")).toContainText("3 videos");
  await expect(page.getByTestId("cut-meta")).toContainText("3 parts");

  // A topic on alpha's words, then on bravo's (the transcript shows one video at a time).
  await openTab(page, "Transcript"); // the side panel opens on Look
  const words = page.getByTestId("transcript").locator(".w");
  const label = async (title: string) => {
    await expect.poll(() => words.count(), { timeout: 60_000 }).toBeGreaterThan(10);
    await words.nth(0).click();
    await words.nth(8).click({ modifiers: ["Shift"] });
    await page.keyboard.press("t");
    await page.getByTestId("topic-title").fill(title);
    await page.getByTestId("topic-title").press("Enter");
    await expect(map).toContainText(title);
  };
  await label("Alpha bit");
  await page.getByTestId("transcript-source").selectOption({ label: "bravo" });
  await label("Bravo bit");
  const sec = map.getByTestId("map-stream").locator("..");
  await expect(sec).toContainText("Topics (2)");
  await expect(sec).toContainText("Alpha bit");
  await expect(sec).toContainText("bravo");

  // A long-form from both videos' topics.
  await map.getByTestId("longform-new").click();
  const lf = page.getByTestId("longform-sheet");
  await expect(lf.getByRole("checkbox")).toHaveCount(2);
  await lf.getByRole("checkbox").nth(0).check();
  await lf.getByRole("checkbox").nth(1).check();
  await lf.getByTestId("longform-make").click();
  await expect(page.getByTestId("cut-title")).toHaveText("Alpha bit", { timeout: 20_000 });
  await expect(page.getByTestId("cut-meta")).toContainText("2 parts");

  // charlie's words, from the transcript's source picker, become a third part.
  const picker = page.getByTestId("transcript-source");
  await expect(picker).toBeVisible();
  await picker.selectOption({ label: "charlie (not in this cut)" });
  await expect.poll(() => words.count(), { timeout: 30_000 }).toBeGreaterThan(10);
  await words.nth(2).click();
  await words.nth(9).click({ modifiers: ["Shift"] });
  await page.getByTestId("add-part").click();
  await expect(page.getByTestId("cut-meta")).toContainText("3 parts", { timeout: 20_000 });

  // The cut mixing three videos exports.
  await page.getByTestId("export").click();
  await expect(page.locator(".export-state")).toContainText("Exported", { timeout: 300_000 });

  // charlie cannot leave the project while the long-form plays it.
  await rows.nth(2).getByTestId("remove-video").click();
  await expect(rows.nth(2).getByTestId("remove-refused")).toContainText("Alpha bit");
  expect(errors).toEqual([]);
});
