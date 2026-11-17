// Open huge files instantly (lane K5): a 2 h 13 min recording (720p H.264, 48 kHz) shows
// its cut and first frame within seconds, before any scrub proxy exists; the transport
// says "Proxy building… scrubbing reads the original"; a scrub to the middle shows the
// master's keyframe at once and the exact frame when it rests. (The timeline filmstrip's
// master keyframes are app-only, gm://data: `perf --open` measures them.)
import { expect, test } from "@playwright/test";
import { existsSync } from "node:fs";
import { bridge, open } from "./qa-lib";
import { labMedia } from "./lab";

const LONG = labMedia("preview/loc_684c44b6.proxy.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(LONG), "no long recording on this machine");

interface Vs {
  frame: number;
  frames: number;
  rendered: number;
  scrub_path: string | null;
}

test("a 2 h recording plays and scrubs before its proxy exists", async ({ browser }) => {
  test.setTimeout(180_000);
  const { page, errors } = await open(browser);
  const t0 = Date.now();
  await page.getByLabel("Video path").fill(LONG);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("loc_684c44b6.proxy", { timeout: 30_000 });
  await expect.poll(async () => (await bridge<Vs>(page, "viewer_state", {})).rendered, { timeout: 30_000, intervals: [50] }).toBeGreaterThan(0);
  const firstMs = Date.now() - t0;
  console.log(`first frame ${firstMs} ms after Enter`);
  expect(firstMs).toBeLessThan(10_000);
  const chip = page.getByTestId("proxy-building");
  await expect(chip).toBeVisible();
  await expect(chip).toContainText("Proxy building… scrubbing reads the original");
  const vs = await bridge<Vs>(page, "viewer_state", {});
  expect(vs.frames).toBeGreaterThan(2 * 3600 * 30);
  // the middle, by a scrub step: the master's keyframe, then the exact frame
  const mid = Math.floor(vs.frames / 2);
  await bridge(page, "viewer_seek", { frame: mid, scrub: true });
  await expect.poll(async () => (await bridge<Vs>(page, "viewer_state", {})).frame, { timeout: 10_000 }).toBe(mid);
  await expect(page.getByTestId("timecode")).toHaveAttribute("data-scrub-path", /keyframe|exact/);
  await expect(page.getByTestId("timecode")).toHaveAttribute("data-scrub-path", "exact", { timeout: 10_000 });
  // narrow: the chip keeps a short form
  await page.setViewportSize({ width: 480, height: 360 });
  await expect(chip).toContainText(/Proxy \d+%/);
  expect(errors).toEqual([]);
});
