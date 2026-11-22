// Before/after screenshots of every main screen for a UI polish pass, in dark and light:
// Home, Agents, Settings, Search, the cut, the Captions (Short) tab, the keys help, the
// palette and the export options. Runs only with GMCLIP_POLISH_SHOTS=<label>; the video is
// GMCLIP_POLISH_VIDEO (any short clip with speech), the folder GMCLIP_POLISH_OUT:
//   GMCLIP_POLISH_SHOTS=before GMCLIP_POLISH_VIDEO=~/Movies/x.mp4 GMCLIP_POLISH_OUT=/tmp/shots \
//     node scripts/e2e.mjs polishshots
// writes <out>/<label>/<screen>-<theme>.png at 1440 × 900.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, extname, join, resolve } from "node:path";
import { openPage } from "./ux-lib";
import { openTab } from "./tabs";
import { bridge } from "./qa-lib";

const LABEL = process.env.GMCLIP_POLISH_SHOTS ?? "";
const VIDEO = process.env.GMCLIP_POLISH_VIDEO ?? "";
const OUT = resolve(process.env.GMCLIP_POLISH_OUT ?? "test-results/polish", LABEL);

test.skip(!LABEL, "screenshots only with GMCLIP_POLISH_SHOTS=<label>");
test.skip(!VIDEO || !existsSync(VIDEO), "GMCLIP_POLISH_VIDEO names no file");

async function both(page: Page, name: string) {
  await page.mouse.move(1, 450);
  for (const scheme of ["dark", "light"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.evaluate(() => document.fonts.ready);
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(OUT, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "dark" });
}

test("polish screenshots", async ({ browser }) => {
  test.setTimeout(600_000);
  mkdirSync(OUT, { recursive: true });
  const { page } = await openPage(browser, { width: 1440, height: 900 });
  await expect(page.getByLabel("Video path")).toBeVisible();
  await both(page, "01-home");

  await page.getByTestId("nav-agents").click();
  await page.waitForTimeout(800);
  await both(page, "02-agents");

  if (process.env.GMCLIP_POLISH_DEBUG) {
    const t0 = Date.now();
    const r = await Promise.race([bridge(page, "owner_settings", {}).then((x) => JSON.stringify(x).slice(0, 300)).catch((e) => String(e)), new Promise((ok) => setTimeout(() => ok("TIMEOUT 15s"), 15_000))]);
    console.log(`owner_settings ${Date.now() - t0} ms: ${r}`);
  }
  await page.getByRole("button", { name: "Settings" }).first().click();
  await page.waitForTimeout(800);
  await both(page, "03-settings");
  // Settings › Recording (off: no permission rows), from the section list when there is one
  await page.getByTestId("settings-nav-recording").click({ timeout: 2000 }).catch(() => undefined);
  await page.waitForTimeout(700);
  await both(page, "03b-settings-recording");

  await page.keyboard.press("ControlOrMeta+Shift+f");
  await page.waitForTimeout(800);
  await both(page, "04-search");

  await page.getByRole("button", { name: "Home" }).first().click();
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-polish-")), `demo-clip${extname(VIDEO)}`);
  copyFileSync(VIDEO, copy);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  // a file the Mac cannot play as is (HEVC, several tracks) asks first: the import sheet
  const sheet = page.getByTestId("import-open");
  await expect(sheet.or(page.getByTestId("cut-title")).first()).toBeVisible({ timeout: 30_000 });
  if (await sheet.isVisible()) {
    await both(page, "05-import");
    await sheet.click();
  }
  await expect(page.getByTestId("cut-title")).toHaveText(basename(copy, extname(copy)), { timeout: 30_000 });
  // the words in: the transcript tab shows them
  await openTab(page, "Words");
  await expect(page.getByTestId("transcript").locator(".w").first()).toBeAttached({ timeout: 180_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await openTab(page, "Captions");
  await page.waitForTimeout(3000);
  await both(page, "06-cut-first-run");
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }).catch(() => undefined);
  await page.waitForTimeout(500);
  await both(page, "07-cut");
  // the status pill opened: saved, exported, the cut's checks
  await page.getByTestId("checks").locator("summary").first().click();
  await page.waitForTimeout(500);
  if (process.env.GMCLIP_POLISH_DEBUG) console.log(`checks: ${await page.getByTestId("checks").evaluate((e) => [...e.querySelectorAll("li")].map((l) => `${l.getAttribute("data-rule") ?? ""} ${l.textContent}`).join("\n"))}`);
  await both(page, "07b-cut-status");
  await page.getByTestId("checks").locator("summary").first().click();

  await openTab(page, "Words");
  await page.waitForTimeout(800);
  await both(page, "07c-words");
  await openTab(page, "Short");
  await page.waitForTimeout(800);
  await both(page, "08-short");
  await openTab(page, "Look");
  await page.waitForTimeout(800);
  await both(page, "09-look");
  await openTab(page, "Words");

  await page.getByRole("button", { name: "Export options" }).click();
  await page.waitForTimeout(600);
  await both(page, "10-export");
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Export options" }).click().catch(() => undefined);

  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: /Keyboard/ })).toBeVisible();
  await both(page, "11-keys");
  await page.keyboard.press("Escape");

  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("export");
  await both(page, "12-palette");
  await page.getByTestId("palette-input").press("Escape");

  // the project map with more in it: three topics on the main cut, a long video from them
  // and a short from a moment (the sidebar tree)
  const pid = (await bridge<{ id: string; name: string }[]>(page, "project_list_open", {})).find((p) => p.name.includes("demo-clip"))!.id;
  const stream = (await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid)).cut;
  const tv = await bridge<{ start_ms: number[] }>(page, "transcript_view", { cut: stream }, pid);
  const at = (sec: number) => Math.max(0, tv.start_ms.findIndex((x) => x >= sec * 1000));
  const topics: string[] = [];
  for (const [a, b, title] of [[0, 9, "Why stop using harnesses"], [10, 20, "What BB is"], [21, 40, "Real Claude Code and Codex"]] as const) {
    topics.push((await bridge<{ topic: string }>(page, "topic_label_ord", { cut: stream, from: at(a), to: Math.max(at(a), at(b) - 1), title }, pid)).topic);
  }
  await bridge(page, "longform_from_topics", { topics, order: "source", title: "BB explained (long)" }, pid);
  const m1 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 300, out_frame: 750 }, pid);
  const short = (await bridge<{ cut: string }>(page, "cut_from_moment", { moment: m1.moment.id }, pid)).cut;
  await bridge(page, "cut_title_set", { cut: short, title: "It's called BB" }, pid).catch(() => undefined);
  await page.getByTestId("map-stream").click();
  await page.waitForTimeout(1500);
  await both(page, "14-tree-main-cut");
  await page.getByTestId("map-short").first().locator(".map-open").click();
  await page.waitForTimeout(1500);
  await both(page, "15-tree-short");
  // (a round before the tree has no ⋯ menu: the shot shows the map as it was)
  await page.getByTestId("video-menu").first().click({ timeout: 3000 }).catch(() => undefined);
  await page.waitForTimeout(400);
  await both(page, "16-tree-video-menu");
  await page.keyboard.press("Escape");
  void short;
  if (process.env.GMCLIP_POLISH_DEBUG) console.log("retakes", JSON.stringify(await bridge(page, "sweep_preview", { cut: stream, kind: "retakes", min_s: 0.4, keep_s: 0.1 }, pid).catch((e) => String(e))).slice(0, 800));
  // the Words panel as prose: the main cut with its topics, then the short
  await page.getByTestId("map-stream").click();
  await openTab(page, "Words");
  await page.waitForTimeout(1500);
  await both(page, "17-words-main");
  await page.getByTestId("map-short").first().locator(".map-open").click();
  await openTab(page, "Words");
  await page.waitForTimeout(1500);
  await both(page, "18-words-short");

  await page.getByRole("button", { name: "Home" }).first().click();
  await page.waitForTimeout(1200);
  await both(page, "13-home-with-project");
  await page.close();
});
