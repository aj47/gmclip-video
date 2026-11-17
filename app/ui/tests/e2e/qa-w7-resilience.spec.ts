// QA workflow 7 (milestones/m2/qa-e2e.md): resilience on the real stream.
// 1. Edits, "Saved", one more edit, then the app is killed (kill -9, mid-edit).
// 2. Relaunched on the same home: the project reopens with every saved edit; "Saved" was
//    honest; the first ⌘Z after reopening asks once, then undoes; ⌘⇧Z redoes.
// 3. Every panel at 800×600 and 600×900: nothing sideways off the window.
// 4. Low disk, for real: the project's database and its export folder on a 300 MB disk
//    image. An export is refused with the numbers; a full disk shows "Not saved" and a
//    freed one "Saved" again, with the edit kept.
import { expect, test, type Browser, type Page } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { bridge, focusCut, frameOf, has, note, open, openVideo, projectNamed, QA, STREAM_A, timed } from "./qa-lib";
import { openTab } from "./tabs";

test.skip(!QA || !has(STREAM_A), "GMCLIP_QA=1 and the loc_4fadb756 preview");

const NAME = "loc_4fadb756";
interface CutV { frames: number; parts: { id: string }[]; captions: { role: string }[]; framing?: { aspect: string } }

/** A second launch of the app on `home` (as global-setup launches the first). */
async function launch(home: string): Promise<{ port: number; token: string; kill: (sig?: NodeJS.Signals) => void }> {
  const exe = process.platform === "win32" ? ".exe" : "";
  const bin = resolve(process.env.GMCLIP_APP_BIN ?? `../src-tauri/target/debug/gmclip-app${exe}`);
  const tmp = mkdtempSync(join(tmpdir(), "gmclip-qa-w7-"));
  const origin = `http://127.0.0.1:${process.env.GMCLIP_E2E_PORT ?? 4173}`;
  // Windows: the profile and app data under `home` too, as global-setup does
  const win = process.platform === "win32"
    ? { USERPROFILE: home, LOCALAPPDATA: join(home, "AppData", "Local"), TEMP: tmp, TMP: tmp }
    : {};
  const child = spawn(bin, ["--dev-bridge", "--no-window", "--dev-bridge-origin", origin], {
    env: { ...process.env, HOME: home, TMPDIR: tmp, ...win, GMCLIP_PLAYBACK_SILENT: "1" },
    stdio: ["ignore", "pipe", "inherit"],
  });
  const info = await new Promise<{ port: number; token: string }>((ok, fail) => {
    let buf = "";
    child.stdout.on("data", (d: Buffer) => {
      buf += d.toString();
      const m = /GMCLIP_DEV_BRIDGE (\{.*\})/.exec(buf);
      if (m?.[1]) ok(JSON.parse(m[1]));
    });
    child.on("exit", (c) => fail(new Error(`gmclip-app exited (${c})`)));
  });
  return { ...info, kill: (sig: NodeJS.Signals = "SIGTERM") => child.kill(sig) };
}

async function page2(browser: Browser, info: { port: number; token: string }): Promise<{ page: Page; errors: string[] }> {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((i) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = i;
  }, info);
  await page.goto("/");
  return { page, errors };
}

test("W7: kill mid-edit, reopen, undo across sessions, small windows, low disk", async ({ browser }) => {
  test.setTimeout(1_500_000);
  const { page, state } = await open(browser);
  await openVideo(page, STREAM_A, NAME);
  const pid = (await projectNamed(page, NAME))!.id;
  const cut = (await projectNamed(page, NAME))!.cuts[0]!.id;
  const view = (p: Page, project = pid) => bridge<CutV>(p, "cut_view", { cut }, project);
  // edits: trim to 43:30–44:30, 9:16, captions, a strike
  for (const [f, k] of [[2610 * 30, "i"], [60 * 30, "o"]] as const) {
    await bridge(page, "viewer_seek", { frame: f });
    await expect.poll(() => frameOf(page)).toBe(f);
    await focusCut(page);
    await page.keyboard.press(k);
  }
  await expect.poll(async () => (await view(page)).frames).toBe(1800);
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Short");
  await page.getByTestId("captions-on").click();
  await expect.poll(async () => (await view(page)).captions.length, { timeout: 30_000 }).toBeGreaterThan(5);
  await openTab(page, "Transcript");
  const kept = page.getByTestId("transcript").locator(".w:not(.out)");
  await kept.nth(10).click();
  await kept.nth(12).click({ modifiers: ["Shift"] });
  await focusCut(page);
  await page.keyboard.press("d");
  await expect(page.getByTestId("saved")).toHaveText("Saved", { timeout: 20_000 });
  const savedState = await view(page);
  // one more strike, then kill -9 at once (mid-edit)
  await kept.nth(30).click();
  await focusCut(page);
  await page.keyboard.press("d");
  const chip = await page.getByTestId("saved").textContent();
  process.kill(state.pid, "SIGKILL");
  note(`W7 killed with the chip at "${chip}"; saved ${savedState.frames} frames, then one more strike`);
  await page.close();

  // 2. relaunch on the same home
  const app2 = await timed("relaunch after kill -9", () => launch(state.home));
  try {
    const { page: p, errors } = await page2(browser, app2);
    await timed("reopen the project", () => openVideo(p, STREAM_A, NAME, 60_000));
    const pid2 = (await projectNamed(p, NAME))!.id;
    const v2 = await view(p, pid2);
    note(`W7 reopened: ${v2.frames} frames, ${v2.parts.length} parts, ${v2.captions.length} captions, ${v2.framing?.aspect}`);
    // everything "Saved" came back; the last edit too if the chip said Saved when killed
    expect(v2.framing?.aspect).toBe("9:16");
    expect(v2.captions.length).toBeGreaterThan(5);
    // the saved edits are all there; the strike made as the app died either landed whole
    // (a word shorter) or not at all
    expect(v2.frames).toBeLessThanOrEqual(savedState.frames);
    expect(savedState.frames - v2.frames).toBeLessThan(60);
    note(`W7 the in-flight strike ${v2.frames < savedState.frames ? "landed" : "was lost"} (the chip said "${chip}")`);
    await expect(p.getByTestId("saved")).toHaveText("Saved");
    // the first ⌘Z after reopening asks, then undoes; ⌘⇧Z redoes
    await focusCut(p);
    await p.keyboard.press("ControlOrMeta+z");
    const ask = p.getByRole("alertdialog", { name: "Undo from an earlier session" });
    await expect(ask).toBeVisible({ timeout: 10_000 });
    note(`W7 undo ask: ${await ask.textContent()}`);
    await ask.getByTestId("undo-ask-yes").click();
    await expect.poll(async () => (await view(p, pid2)).frames, { timeout: 20_000 }).not.toBe(v2.frames);
    const undone = (await view(p, pid2)).frames;
    await focusCut(p);
    await p.keyboard.press("ControlOrMeta+Shift+z");
    const redoAsk = p.getByRole("alertdialog", { name: "Undo from an earlier session" });
    if (await redoAsk.isVisible({ timeout: 2_000 }).catch(() => false)) await redoAsk.getByTestId("undo-ask-yes").click();
    await expect.poll(async () => (await view(p, pid2)).frames, { timeout: 20_000 }).toBe(v2.frames);
    note(`W7 undo/redo across sessions: ${v2.frames} → ${undone} → ${v2.frames}`);

    // 3. every panel at 800×600 and 600×900
    const tabs = ["Transcript", "Inspector", "Layout", "Short", "Tighten", "Agent", "Story", "Review", "Thumbnail"];
    const bad: string[] = [];
    for (const vp of [{ width: 800, height: 600 }, { width: 600, height: 900 }]) {
      await p.setViewportSize(vp);
      await p.waitForTimeout(300);
      const show = p.getByRole("button", { name: "Show the side panel" });
      if (await show.count()) await show.click();
      for (const t of tabs) {
        try {
          await openTab(p, t);
        } catch {
          bad.push(`${vp.width}×${vp.height} ${t}: not reachable from its group`);
          await p.keyboard.press("Escape");
          continue;
        }
        await p.waitForTimeout(150);
        const r = await p.evaluate(() => {
          const sw = document.documentElement.scrollWidth;
          const panel = document.querySelector("[role=tabpanel], .side-pane, .side")?.getBoundingClientRect();
          return { sw, right: panel ? panel.right : 0, w: innerWidth };
        });
        if (r.sw > r.w + 1 || r.right > r.w + 1) bad.push(`${vp.width}×${vp.height} ${t}: page ${r.sw} px, panel right ${Math.round(r.right)} px`);
      }
      for (const [label, go] of [["Home", () => p.getByRole("button", { name: "Home" }).first().click()], ["Settings", () => p.getByRole("button", { name: "Settings" }).first().click()]] as const) {
        await go();
        await p.waitForTimeout(300);
        const sw = await p.evaluate(() => document.documentElement.scrollWidth);
        if (sw > vp.width + 1) bad.push(`${vp.width}×${vp.height} ${label}: page ${sw} px`);
      }
      await p.getByRole("button", { name: "Home" }).first().click();
      await openVideo(p, STREAM_A, NAME, 60_000);
    }
    note(`W7 small windows: ${bad.length ? bad.join("; ") : "every panel fits"}`);
    await p.setViewportSize({ width: 1440, height: 900 });
    expect(bad).toEqual([]);
    expect(errors).toEqual([]);
  } finally {
    app2.kill();
  }

  // 4. low disk, for real: a 300 MB disk image under the project's database and exports
  const scratch = mkdtempSync(join(tmpdir(), "gmclip-qa-disk-"));
  const dmg = join(scratch, "small.dmg");
  const mnt = join(scratch, "mnt");
  mkdirSync(mnt);
  execFileSync("hdiutil", ["create", "-size", "300m", "-fs", "APFS", "-volname", "gmclipqa", dmg]);
  execFileSync("hdiutil", ["attach", "-nobrowse", "-mountpoint", mnt, dmg]);
  const appData = join(state.home, "Library/Application Support/gmclip");
  const projects = join(appData, "projects");
  const exportsDir = join(state.home, "Movies/gmclip", NAME);
  await new Promise((r) => setTimeout(r, 1500)); // the second app has let go of the database
  execFileSync("cp", ["-R", projects, join(mnt, "projects")]);
  rmSync(projects, { recursive: true, force: true });
  symlinkSync(join(mnt, "projects"), projects);
  rmSync(exportsDir, { recursive: true, force: true });
  mkdirSync(join(mnt, "exports"));
  symlinkSync(join(mnt, "exports"), exportsDir);
  const app3 = await launch(state.home);
  try {
    const { page: p } = await page2(browser, app3);
    await openVideo(p, STREAM_A, NAME, 60_000);
    const pid3 = (await projectNamed(p, NAME))!.id;
    // an export with < 2 GB free is refused, with the numbers
    await p.getByTestId("export").click();
    const toast = p.locator(".toast.error");
    await expect(toast).toContainText("Not enough free space", { timeout: 20_000 });
    note(`W7 low disk export: ${await toast.textContent()}`);
    // fill the disk: the next edit cannot be saved, and says so
    const fill = join(mnt, "filler");
    try {
      execFileSync("dd", ["if=/dev/zero", `of=${fill}`, "bs=1m", "count=400"], { stdio: "ignore" });
    } catch {
      /* stops when the disk is full */
    }
    try {
      execFileSync("dd", ["if=/dev/zero", `of=${fill}-2`, "bs=4k", "count=100000"], { stdio: "ignore" });
    } catch {
      /* the last few blocks */
    }
    note(`W7 disk after filling: ${execFileSync("df", ["-k", mnt], { encoding: "utf8" }).split("\n")[1]}`);
    const before = (await view(p, pid3)).frames;
    await openTab(p, "Transcript");
    const k3 = p.getByTestId("transcript").locator(".w:not(.out)");
    await k3.nth(40).click();
    await focusCut(p);
    await p.keyboard.press("d");
    // the chip settles: "Saved" must mean on disk (a write into the database's existing
    // pages can still succeed on a full disk), "Not saved" must clear once there is room
    await expect(p.getByTestId("saved")).toHaveText(/^(Saved|Not saved.*)$/, { timeout: 30_000 });
    const chip4 = (await p.getByTestId("saved").textContent()) ?? "";
    note(`W7 full disk: chip "${chip4}", toasts ${(await p.locator(".toast").allTextContents()).join(" | ")}`);
    if (/Not saved/.test(chip4)) {
      rmSync(fill, { force: true });
      rmSync(`${fill}-2`, { force: true });
      await timed("space back: Saved again", async () => {
        await expect(p.getByTestId("saved")).toHaveText("Saved", { timeout: 60_000 });
      });
    }
    const after = (await view(p, pid3)).frames;
    expect(after).toBeLessThan(before);
    // proof: kill -9 and look again
    app3.kill("SIGKILL");
    await p.close();
    rmSync(fill, { force: true });
    rmSync(`${fill}-2`, { force: true });
    await new Promise((r) => setTimeout(r, 1000));
    const app4 = await launch(state.home);
    try {
      const { page: q } = await page2(browser, app4);
      await openVideo(q, STREAM_A, NAME, 60_000);
      const pid4 = (await projectNamed(q, NAME))!.id;
      const got = (await view(q, pid4)).frames;
      note(`W7 after kill -9 on the full disk: ${got} frames (the chip said "${chip4}" at ${after})`);
      if (chip4 === "Saved") expect(got).toBe(after);
    } finally {
      app4.kill();
    }
  } finally {
    app3.kill();
    await new Promise((r) => setTimeout(r, 1500));
    try {
      execFileSync("hdiutil", ["detach", mnt, "-force"]);
    } catch {
      /* already gone */
    }
    rmSync(scratch, { recursive: true, force: true });
  }
});
