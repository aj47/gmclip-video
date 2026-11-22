// Screenshots for gmclip.com and the README, from a real demo project, in dark and light:
// Home, the cut at 9:16 and 16:9, the cut with topics in the tree, Captions, Agents, the
// keys help and the palette. Runs only with GMCLIP_SITE_SHOTS=<out folder>; the video is
// GMCLIP_SITE_VIDEO (a talking Short), opened under the name GMCLIP_SITE_NAME:
//   GMCLIP_SITE_SHOTS=/tmp/shots GMCLIP_SITE_VIDEO=~/x.mp4 GMCLIP_SITE_NAME="exe.dev — VMs" \
//     node scripts/e2e.mjs siteshots
// writes <out>/<screen>-<theme>.png at 1280 × 800 CSS px, 1.5625 device scale (2000 × 1250).
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join } from "node:path";
import { STATE } from "./global-setup";
import { openTab, rowOn } from "./tabs";
import { bridge } from "./qa-lib";

const OUT = process.env.GMCLIP_SITE_SHOTS ?? "";
const VIDEO = process.env.GMCLIP_SITE_VIDEO ?? "";
const NAME = process.env.GMCLIP_SITE_NAME ?? "demo";
const PROBE = !!process.env.GMCLIP_SITE_PROBE;

test.skip(!OUT, "screenshots only with GMCLIP_SITE_SHOTS=<folder>");
test.skip(!VIDEO || !existsSync(VIDEO), "GMCLIP_SITE_VIDEO names no file");

async function both(page: Page, name: string) {
  if (PROBE) return;
  await page.mouse.move(1, 790);
  for (const scheme of ["dark", "light"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.evaluate(() => document.fonts.ready);
    await page.waitForTimeout(700);
    await page.screenshot({ path: join(OUT, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "light" });
}

test("site screenshots", async ({ browser }) => {
  test.setTimeout(900_000);
  mkdirSync(OUT, { recursive: true });
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5625 });
  const page = await ctx.newPage();
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByLabel("Video path")).toBeVisible();
  // the welcome banner is for a first run: dismissed, as a person who set up would have
  await page.getByRole("button", { name: "Dismiss" }).click({ timeout: 3000 }).catch(() => undefined);

  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-site-")), `${NAME}${extname(VIDEO)}`);
  copyFileSync(VIDEO, copy);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  const sheet = page.getByTestId("import-open");
  await expect(sheet.or(page.getByTestId("cut-title")).first()).toBeVisible({ timeout: 30_000 });
  if (await sheet.isVisible()) await sheet.click();
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  await openTab(page, "Words");
  await expect(page.getByTestId("transcript").locator(".w").first()).toBeAttached({ timeout: 240_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 240_000 });

  const pid = (await bridge<{ id: string; name: string }[]>(page, "project_list_open", {})).find((p) => p.name.includes(NAME.slice(0, 6)))!.id;
  const open = await bridge<{ id: string; name: string; cuts: { id: string; title: string }[] }[]>(page, "project_list_open", {});
  const cut = open.find((p) => p.id === pid)!.cuts[0]!.id;
  type Tv = { text: string[]; start_ms: number[]; frame: number[]; owner: string; lines: [number, number][] };
  const tv = await bridge<Tv>(page, "transcript_view", { cut }, pid);
  if (PROBE) {
    const cv = await bridge<Record<string, unknown>>(page, "cut_view", { cut }, pid);
    writeFileSync(join(OUT, "probe.json"), JSON.stringify({ cv, tv }, null, 1));
    return;
  }
  // the brand the speech model mis-hears ("XC.dev"), spelt right in the words and captions
  const misheard = (w: string) => /^[a-z]{1,3}\.dev[.,]?$/i.test(w) && !/^exe\.dev/i.test(w);
  for (const [i, w] of tv.text.entries()) {
    if (misheard(w)) await bridge(page, "word_respell", { cut, word: i, text: w.replace(/^[a-z]{1,3}\.dev/i, "exe.dev"), owner: tv.owner }, pid);
  }
  // eight parts, each starting on a word
  const fps = 30;
  for (const t of [53, 47, 36, 27, 23, 14, 6]) {
    const i = tv.start_ms.findIndex((ms) => ms >= t * 1000);
    if (i > 0) await bridge(page, "cut_split", { cut, frame: Math.round((tv.start_ms[i]! * fps) / 1000) }, pid).catch(() => undefined);
  }
  // a text hook (the Short has its captions burned in already: gmclip's stay off)
  await openTab(page, "Captions");
  await rowOn(page, "hook");
  await page.getByLabel("Hook text").fill("50 VMs for $20 a month");
  await page.getByLabel("Hook text").press("Enter");
  await page.waitForTimeout(1500);

  const seek = async (sec: number) => {
    await bridge(page, "viewer_seek", { frame: Math.round(sec * fps) }, pid).catch(() => undefined);
    await page.waitForTimeout(1500);
  };
  await seek(4.5);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }).catch(() => undefined);
  await both(page, "captions");

  await openTab(page, "Words");
  await seek(35);
  await both(page, "editor-vertical");

  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "16:9" }).click();
  await page.waitForTimeout(2500);
  await seek(35);
  await both(page, "editor-horizontal");
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await page.waitForTimeout(1500);

  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: /Keyboard/ })).toBeVisible();
  await both(page, "keys");
  await page.keyboard.press("Escape");

  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("export");
  await page.waitForTimeout(500);
  await both(page, "palette");
  await page.getByTestId("palette-input").press("Escape");

  // the project map with topics, a long-form from them and a short from a moment
  const stream = (await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid)).cut;
  const sv = await bridge<{ start_ms: number[] }>(page, "transcript_view", { cut: stream }, pid);
  const at = (sec: number) => Math.max(0, sv.start_ms.findIndex((x) => x >= sec * 1000));
  const topics: string[] = [];
  for (const [a, b, title] of [[0, 13, "Fifty VMs for twenty dollars"], [14, 35, "Shelley sets them up"], [36, 64, "A proxy pool from one prompt"]] as const) {
    const to = b >= 64 ? sv.start_ms.length - 1 : Math.max(at(a), at(b) - 1);
    topics.push((await bridge<{ topic: string }>(page, "topic_label_ord", { cut: stream, from: at(a), to, title }, pid)).topic);
  }
  await bridge(page, "longform_from_topics", { topics, order: "source", title: "exe.dev explained" }, pid);
  const m1 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 420, out_frame: 1080 }, pid);
  await bridge<{ cut: string }>(page, "cut_from_moment", { moment: m1.moment.id }, pid);
  await page.getByTestId("map-stream").click();
  await openTab(page, "Words");
  await page.waitForTimeout(2000);
  await seek(20);
  await both(page, "editor-topics");

  await page.getByTestId("nav-agents").click();
  await page.waitForTimeout(1500);
  await both(page, "agents");

  await page.getByRole("button", { name: "Home" }).first().click();
  await page.waitForTimeout(1500);
  // the first-short checklist is a first run's: dismissed
  await page.getByRole("button", { name: "Dismiss" }).first().click({ timeout: 3000 }).catch(() => undefined);
  await page.waitForTimeout(800);
  await both(page, "home");
  await ctx.close();
});
