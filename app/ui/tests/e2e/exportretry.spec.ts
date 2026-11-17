// H.06 rest: the cut says what the export does to loudness (a portrait video starts at
// the Reels preset, −16 LUFS static) and can change it; a failed export names its step
// and Retry runs it again. R-EXP-4: a decode failure (the video's samples zeroed from the
// middle on) names the decoder's step and the output frame, and Retry, with the file put
// back, exports.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("loudness preset, failed export, retry", async ({ browser }) => {
  test.setTimeout(240_000);
  const dir = mkdtempSync(join(tmpdir(), "gmclip-retry-"));
  const copy = join(dir, "retry-me.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("retry-me", { timeout: 30_000 });
  await expect(page.getByTestId("loudness")).toContainText("Loudness: -16 LUFS static, peaks ≤ -1.5 dBTP");
  await page.getByRole("button", { name: "Export options" }).click();
  await page.getByLabel("Export loudness").selectOption("as_recorded");
  await page.getByRole("button", { name: "Export options" }).click();
  await expect(page.getByTestId("loudness")).toContainText("Loudness: as recorded", { timeout: 10_000 });

  // the source goes missing: the export fails and says where
  renameSync(copy, copy + ".away");
  await page.getByTestId("export").click();
  await expect(page.getByTestId("export-failed")).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId("export-failed")).toContainText(/open_source|demux|source/);
  // put it back and retry
  renameSync(copy + ".away", copy);
  await page.getByTestId("export-failed").getByRole("button", { name: "Retry" }).click();
  await expect(page.locator(".export-state")).toContainText("Exported", { timeout: 180_000 });
  await expect(page.getByTestId("export-failed")).toHaveCount(0);
  await expect(page.getByTestId("loudness")).toContainText("last export", { timeout: 10_000 });
  expect(errors).toEqual([]);
});

/** The video track's samples of an MP4 (`moov/trak` whose handler is `vide`): byte
 * offset and size of each, in decode order (stsz, stsc, stco/co64). */
function videoSamples(b: Buffer): { off: number; size: number }[] {
  const boxes = (from: number, to: number) => {
    const out: { type: string; at: number; end: number }[] = [];
    for (let at = from; at + 8 <= to; ) {
      let n = b.readUInt32BE(at);
      let head = 8;
      if (n === 1) {
        n = Number(b.readBigUInt64BE(at + 8));
        head = 16;
      }
      if (n < head) break;
      out.push({ type: b.toString("latin1", at + 4, at + 8), at: at + head, end: at + n });
      at += n;
    }
    return out;
  };
  const child = (p: { at: number; end: number }, t: string) => boxes(p.at, p.end).find((x) => x.type === t);
  const moov = boxes(0, b.length).find((x) => x.type === "moov")!;
  for (const trak of boxes(moov.at, moov.end).filter((x) => x.type === "trak")) {
    const mdia = child(trak, "mdia")!;
    const hdlr = child(mdia, "hdlr")!;
    if (b.toString("latin1", hdlr.at + 8, hdlr.at + 12) !== "vide") continue;
    const stbl = child(child(mdia, "minf")!, "stbl")!;
    const stsz = child(stbl, "stsz")!;
    const fixed = b.readUInt32BE(stsz.at + 4);
    const count = b.readUInt32BE(stsz.at + 8);
    const sizes = Array.from({ length: count }, (_, i) => fixed || b.readUInt32BE(stsz.at + 12 + 4 * i));
    const co = child(stbl, "stco") ?? child(stbl, "co64")!;
    const wide = b.toString("latin1", co.at - 4, co.at) === "co64";
    const chunks = Array.from({ length: b.readUInt32BE(co.at + 4) }, (_, i) => (wide ? Number(b.readBigUInt64BE(co.at + 8 + 8 * i)) : b.readUInt32BE(co.at + 8 + 4 * i)));
    const stsc = child(stbl, "stsc")!;
    const runs = Array.from({ length: b.readUInt32BE(stsc.at + 4) }, (_, i) => ({ first: b.readUInt32BE(stsc.at + 8 + 12 * i), per: b.readUInt32BE(stsc.at + 12 + 12 * i) }));
    const out: { off: number; size: number }[] = [];
    let s = 0;
    chunks.forEach((c, ci) => {
      const run = [...runs].reverse().find((r) => r.first <= ci + 1)!;
      let off = c;
      for (let k = 0; k < run.per && s < count; k++, s++) {
        out.push({ off, size: sizes[s]! });
        off += sizes[s]!;
      }
    });
    return out;
  }
  throw new Error("no video track");
}

test("a decode failure names its step and frame; Retry exports", async ({ browser }) => {
  test.setTimeout(240_000);
  const dir = mkdtempSync(join(tmpdir(), "gmclip-decodefail-"));
  const copy = join(dir, "decode-me.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("decode-me", { timeout: 30_000 });

  // the picture's samples from the middle of the video on: zeros (the sound stays, so
  // the mix, made first, reads; the decoder fails partway through)
  const good = readFileSync(copy);
  const bad = Buffer.from(good);
  const samples = videoSamples(bad);
  for (const x of samples.slice(samples.length >> 1)) bad.fill(0, x.off, x.off + x.size);
  writeFileSync(copy, bad);
  await page.getByTestId("export").click();
  const failed = page.getByTestId("export-failed");
  await expect(failed).toBeVisible({ timeout: 120_000 });
  const text = (await failed.textContent()) ?? "";
  console.log(`decode failure: ${text}`);
  const m = /(\w+) failed at frame (\d+)/.exec(text);
  expect(m, text).not.toBeNull();
  // VideoToolbox fails the zeroed samples in its decoder; on Windows they fail turning
  // the samples into Annex B for Media Foundation, before a decode
  if (process.platform === "win32") expect(m![1]).toMatch(/^(nal_length_truncated|nal_truncated|mf_decode\w*)$/);
  else expect(m![1]).toBe("vt_decode");
  expect(Number(m![2])).toBeGreaterThan(0);
  // the file back; Retry
  writeFileSync(copy, good);
  await failed.getByRole("button", { name: "Retry" }).click();
  await expect(page.locator(".export-state")).toContainText("Exported", { timeout: 180_000 });
  await expect(failed).toHaveCount(0);
  expect(errors).toEqual([]);
});
