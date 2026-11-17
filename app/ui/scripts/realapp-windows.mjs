// The built Windows app (release, custom-protocol: what the MSI installs) driven through
// its real WebView2 window: WebView2's DevTools port is opened for this one process by
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS (no system setting), and Playwright attaches
// over CDP. Checks the things only a built app can show: the page's origin, the frame
// socket accepting it (viewer frames arrive), the gm:// columns reaching the timeline
// through http://gm.localhost, no console errors (CSP, Trusted Types), and an export
// from the app finishing (GMCLIP_REALAPP_NO_EXPORT skips it).
//
//   node scripts/realapp-windows.mjs <gmclip-app.exe> <video> <log> [<hi-res>]   (from app/ui)
//
// With <hi-res>, <video> is the sound side of a pair ("Open a stream with its hi-res
// picture"): the picture plays from <hi-res>.
//
// The app runs with a throwaway profile (HOME, USERPROFILE, LOCALAPPDATA, APPDATA,
// TEMP, its own WebView2 data folder and agent pipe tag); the speech models are linked
// in read only.
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, appendFileSync, existsSync, symlinkSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "@playwright/test";

const [exe, video, log, hires] = process.argv.slice(2);
const say = (m) => { console.log(m); appendFileSync(log, m + "\n"); };
writeFileSync(log, "");
const home = mkdtempSync(join(tmpdir(), "gmclip-realapp-"));
const local = join(home, "AppData", "Local");
mkdirSync(join(local, "gmclip"), { recursive: true });
const models = join(process.env.LOCALAPPDATA, "gmclip", "models");
if (existsSync(models)) symlinkSync(models, join(local, "gmclip", "models"), "junction");
const port = 9333;
const env = {
  ...process.env,
  USERPROFILE: home, LOCALAPPDATA: local, APPDATA: join(home, "AppData", "Roaming"),
  TEMP: join(home, "tmp"), TMP: join(home, "tmp"), GMCLIP_PIPE_TAG: "realapp",
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  WEBVIEW2_USER_DATA_FOLDER: join(home, "webview2"),
  // playback runs its real clock with the output muted (nothing reaches the speakers)
  GMCLIP_PLAYBACK_SILENT: "1",
};
// as an app started from the Start menu: Windows sets no HOME (shells such as Git Bash do)
delete env.HOME;
mkdirSync(env.TEMP, { recursive: true });
const app = spawn(exe, [], { env, stdio: ["ignore", "pipe", "pipe"] });
app.stdout.on("data", (d) => appendFileSync(log, "[app] " + d));
app.stderr.on("data", (d) => appendFileSync(log, "[app!] " + d));
let ok = false;
try {
  let browser;
  for (let i = 0; i < 60 && !browser; i++) {
    await new Promise((r) => setTimeout(r, 1000));
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`).catch(() => null);
  }
  if (!browser) throw new Error("no WebView2 DevTools endpoint");
  let page;
  for (let i = 0; i < 30 && !page; i++) {
    page = browser.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith("http"));
    if (!page) await new Promise((r) => setTimeout(r, 500));
  }
  if (!page) {
    const urls = browser.contexts().flatMap((c) => c.pages()).map((p) => p.url());
    // `cargo test --release` rebuilds target/release/gmclip-app.exe without custom-protocol
    throw new Error(`no app page (${urls.join(", ") || "none"}): is this a custom-protocol build?`);
  }
  const errors = [];
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  page.on("pageerror", (e) => errors.push(String(e)));
  say(`page origin: ${new URL(page.url()).origin}`);
  await page.getByTestId("open-video").waitFor({ timeout: 30_000 });
  // Home's startup probe (what this PC decodes and encodes)
  await page.getByTestId("mac-cap").first().waitFor({ timeout: 30_000 }).catch(() => {});
  for (const t of await page.getByTestId("mac-cap").allTextContents()) say(`probe: ${t.replace(/\s+/g, " ").trim()}`);
  if (hires) {
    await page.getByTestId("open-pair").click();
    const d = page.getByRole("dialog", { name: "Same recording" });
    await d.getByLabel("Sound file path").fill(video);
    await d.getByLabel("Hi-res file path").fill(hires);
    await d.getByTestId("pair-found").waitFor({ timeout: 120_000 });
    say(`pair: ${(await d.getByTestId("pair-found").innerText()).replace(/\s+/g, " ").slice(0, 300)}`);
    await d.getByTestId("pair-open").click();
  } else {
    await page.getByLabel("Video path").fill(video);
    await page.getByLabel("Video path").press("Enter");
  }
  // a file with notes (e.g. 44.1 kHz audio) asks first on the import sheet: open it
  const sheet = page.getByTestId("import-open");
  const asked = await Promise.race([
    sheet.waitFor({ timeout: 60_000 }).then(() => true, () => false),
    page.getByTestId("cut-title").waitFor({ timeout: 60_000 }).then(() => false, () => false),
  ]);
  if (asked) {
    say("import sheet: opening");
    await sheet.click();
  }
  await page.getByTestId("cut-title").waitFor({ timeout: Number(process.env.REALAPP_OPEN_MS ?? 60_000) }).catch(async (e) => {
    // what the window shows instead (an alert, a sheet, still importing)
    say(`page: ${((await page.locator("body").innerText()) ?? "").replace(/\s+/g, " ").slice(0, 1500)}`);
    throw e;
  });
  for (let i = 0; i < 120 && ((await page.getByTestId("cut-title").textContent()) ?? "").startsWith("Loading"); i++) await new Promise((r) => setTimeout(r, 500));
  say(`cut opened: ${await page.getByTestId("cut-title").textContent()}`);
  // a cut that does not plan says why
  if (await page.getByTestId("cut-error").count()) say(`cut error: ${(await page.getByTestId("cut-error").innerText()).replace(/\s+/g, " ")}`);
  // viewer frames over the frame socket (its exact-origin check passed)
  const stats = page.getByTestId("viewer-stats");
  for (let i = 0; i < 60 && !((await stats.textContent()) ?? "").includes("×"); i++) await new Promise((r) => setTimeout(r, 500));
  say(`viewer: ${(await stats.textContent()) || "(no frames)"}`);
  // the timeline's columns from gm://data (http://gm.localhost on WebView2)
  const tl = page.locator(".timeline-canvas");
  let src = null, pts = 0;
  for (let i = 0; i < 240; i++) {
    src = await tl.getAttribute("data-audio-src").catch(() => null);
    pts = Number(await tl.getAttribute("data-audio-points").catch(() => 0));
    if (src === "columns" && pts > 0) break;
    await new Promise((r) => setTimeout(r, 500));
  }
  say(`timeline audio: src=${src} points=${pts}`);
  // playback: Space plays; the viewer's frame rate while it does
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Space");
  await new Promise((r) => setTimeout(r, 4000));
  const playing = (await stats.textContent()) ?? "";
  await page.keyboard.press("Space");
  const fps = Number((/· (\d+) fps/.exec(playing) ?? [0, 0])[1]);
  say(`playback: ${playing} (${fps} fps)`);
  // an export from the app (the job, the Windows encoders, the export record)
  let exported = false;
  if (!process.env.GMCLIP_REALAPP_NO_EXPORT) {
    await page.getByTestId("export").click();
    const state = page.locator(".export-state");
    const failed = page.getByTestId("export-failed");
    const t0 = Date.now();
    for (let i = 0; i < 1200; i++) {
      const txt = (await state.textContent().catch(() => "")) ?? "";
      if (txt.includes("Exported")) { exported = true; break; }
      if (await failed.isVisible().catch(() => false)) { say(`export failed: ${await failed.textContent()}`); break; }
      await new Promise((r) => setTimeout(r, 500));
    }
    say(`export: ${exported ? "done" : "not done"} in ${((Date.now() - t0) / 1000).toFixed(1)} s: ${((await state.textContent().catch(() => "")) ?? "").slice(0, 200)}`);
  }
  say(`console errors: ${errors.length ? errors.join(" | ") : "none"}`);
  ok = new URL(page.url()).origin === "http://tauri.localhost"
    && ((await stats.textContent()) ?? "").includes("×") && src === "columns" && errors.length === 0
    && (exported || !!process.env.GMCLIP_REALAPP_NO_EXPORT) && fps >= 25;
  await browser.close().catch(() => {});
} catch (e) {
  say(`error: ${e.message}`);
} finally {
  app.kill();
  say(ok ? "RESULT ok" : "RESULT failed");
  // the throwaway profile (WebView2 may hold it a moment after the app ends); rmSync
  // removes the model/font junctions as links, never what they point at
  await new Promise((r) => setTimeout(r, 1500));
  if (!process.env.GMCLIP_REALAPP_KEEP) {
    try {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
    } catch (e) {
      say(`left ${home}: ${e.message}`);
    }
  }
}
process.exit(ok ? 0 : 1);
