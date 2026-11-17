// U.15 essentials: a file with two audio tracks opens through the import sheet —
// capability lines, "Which track is your mic?" with a 3 s preview per track and the
// suggestion by speech ratio; the ETA chain; Open uses the chosen mic; no agent starts
// by itself; reopening does not ask again.
// The two-track file is made here with ffmpeg (a dev tool) from AJ's h79 reel.
import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");
const FF = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].find((p) => existsSync(p));

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO) || !FF, "no test video or ffmpeg on this machine");

test("which track is your mic", async ({ browser }) => {
  test.setTimeout(120_000);
  const two = join(mkdtempSync(join(tmpdir(), "gmclip-two-")), "two-tracks.mp4");
  execFileSync(FF!, ["-v", "error", "-y", "-i", VIDEO, "-f", "lavfi", "-i", "sine=f=440:d=61,volume=0.3", "-map", "0:v", "-map", "1:a", "-map", "0:a", "-c:v", "copy", "-c:a", "aac", "-ar", "48000", "-ac", "2", "-shortest", two]);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(two);
  await page.getByLabel("Video path").press("Enter");

  const sheet = page.getByRole("dialog", { name: "Import" });
  await expect(sheet).toBeVisible({ timeout: 30_000 });
  await expect(sheet.getByTestId("cap-video")).toContainText("H.264 1080×1920: plays and exports");
  const mic = sheet.getByRole("radiogroup", { name: "Which track is your mic?" });
  await expect(mic.getByRole("radio")).toHaveCount(2);
  // the voice (track 2) is suggested and chosen; the tone (track 1) has no speech
  await expect(sheet.getByTestId("track-2")).toContainText("suggested: most speech");
  await expect(sheet.getByTestId("track-2").getByRole("radio")).toBeChecked();
  await expect(sheet.getByTestId("track-1")).toContainText("speech 0%");
  await expect(sheet).toContainText("Transcripts, moment-finding and exports use this track.");
  await sheet.getByRole("button", { name: "Play 3 s of track 2" }).click();
  await sheet.getByRole("button", { name: "Play 3 s of track 1" }).click();
  await expect(sheet.getByText("AAC 48 kHz stereo: transcribed and exported")).toHaveCount(2);
  // the ETA chain (SPEC §12.5): the transcript at this Mac's speed, then the moments when asked
  const chain = sheet.getByTestId("eta-chain");
  await expect(chain).toContainText(/Transcript in (under a minute|~\d+ min)/);
  await expect(chain).toContainText("then your agent finds moments when you ask it");
  await sheet.getByTestId("import-open").click();
  await expect(page.getByTestId("cut-title")).toHaveText("two-tracks", { timeout: 30_000 });
  // transcription hears the mic
  await expect(page.getByTestId("transcript").locator(".w")).not.toHaveCount(0, { timeout: 120_000 });

  // transcript and analysis run by themselves; an agent never starts by itself (D17)
  await page.getByRole("button", { name: "Home" }).click();
  await expect(page.getByTestId("running-agent")).toHaveCount(0);

  // reopening a known file does not ask again
  await page.getByLabel("Video path").fill(two);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("two-tracks", { timeout: 30_000 });
  await expect(sheet).toHaveCount(0);
  expect(errors).toEqual([]);
});
