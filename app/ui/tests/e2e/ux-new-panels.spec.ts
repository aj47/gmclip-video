// AJ 2026-10-17: "10x better", small and tall windows, controls he can find. Every panel
// and sheet the overnight lanes added — the Short tab's openings, hook lines and end card,
// the openings compare sheet, titles and descriptions, the long-form's chapters bar, fork
// and compare sheet, the Polish tab, the Inspector's speed, ramp, plate, backdrop, video
// and covers, Draw your layout, topic results in library search, B-roll, the library-wide Home, the
// export popover's 9:16 + 16:9 and its sheet, the History slider and the keys list — at
// every size from 480 × 360 to 1440 × 900 (a tall 500 × 1000 too): no sideways page
// scroll, the panel inside the window, no control cut off at its panel's edge, the primary
// controls reachable. Each one is reachable from the View menu or ⌘K. Screenshots (dark at
// 1440 × 900, 480 × 360 and 500 × 1000, light at 1440 × 900) go to
// milestones/m2/ui/ux-2026-10-17/; UX_BEFORE=1 names them before-* and only reports.
import { expect, test, type Locator, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { labMedia } from "./lab";
import { openPage, SIZES } from "./ux-lib";
import { openTab, rowOn } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");
const LONG = labMedia("preview/loc_684c44b6.proxy.mp4");
const BEFORE = !!process.env.UX_BEFORE;
const TAG = BEFORE ? "before" : "after";
// The committed screenshots and findings (milestones/m2/ui/ux-2026-10-17) are written only
// for UX_BEFORE=1 or GMCLIP_UPDATE_SHOTS=1: a gate run takes no screenshots and leaves the
// tree clean (its findings go to a scratch folder).
const SHOTS = BEFORE || process.env.GMCLIP_UPDATE_SHOTS === "1";
const DIR = SHOTS ? resolve("../../milestones/m2/ui/ux-2026-10-17") : join(tmpdir(), "gmclip-ux-findings");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");
test.use({ actionTimeout: 30_000 });

type Vp = { width: number; height: number };
const name = (vp: Vp) => `${vp.width}x${vp.height}`;
type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

/** What is wrong with the panel at `root` in this window: the page scrolling sideways,
 * the panel outside the window, the panel scrolling sideways, a control cut off at its
 * panel's (or the window's) edge. */
async function audit(root: Locator, dialog: boolean): Promise<string[]> {
  return root.evaluate((el, isDialog) => {
    const out: string[] = [];
    const W = innerWidth;
    const H = innerHeight;
    if (document.documentElement.scrollWidth > W + 1) out.push(`the page scrolls sideways (${document.documentElement.scrollWidth} > ${W})`);
    const r = el.getBoundingClientRect();
    if (r.left < -1 || r.right > W + 1) out.push(`the panel is ${Math.round(r.left)}–${Math.round(r.right)} in a ${W} px window`);
    if (isDialog && (r.top < -1 || r.bottom > H + 1)) out.push(`the sheet is ${Math.round(r.top)}–${Math.round(r.bottom)} in a ${H} px window`);
    const sx = getComputedStyle(el).overflowX;
    if (el.scrollWidth > el.clientWidth + 1 && sx !== "hidden" && sx !== "clip") out.push(`the panel scrolls sideways (${el.scrollWidth} > ${el.clientWidth})`);
    const right = Math.min(r.right, W);
    const left = Math.max(r.left, 0);
    const controls = el.querySelectorAll<HTMLElement>("button, input, select, textarea, [role=tab], a[href], summary");
    for (const c of controls) {
      const b = c.getBoundingClientRect();
      if (b.width < 2 || b.height < 2) continue; // sr-only, hidden
      const cs = getComputedStyle(c);
      if (cs.visibility === "hidden" || cs.display === "none") continue;
      // in a box that scrolls sideways on purpose (a tab bar, a strip): judged by that box
      let clipR = right;
      let clipL = left;
      for (let p = c.parentElement; p && p !== el; p = p.parentElement) {
        const ox = getComputedStyle(p).overflowX;
        if (ox !== "visible") {
          const pr = p.getBoundingClientRect();
          clipR = Math.min(clipR, pr.right);
          clipL = Math.max(clipL, pr.left);
          if (ox === "auto" || ox === "scroll") { clipR = Infinity; clipL = -Infinity; }
          break;
        }
      }
      if (b.right > clipR + 1 || b.left < clipL - 1) {
        const what = c.getAttribute("aria-label") || c.getAttribute("data-testid") || (c.textContent ?? "").trim().slice(0, 30) || c.tagName.toLowerCase();
        out.push(`“${what}” is cut off (${Math.round(b.left)}–${Math.round(b.right)}, room ${Math.round(clipL)}–${Math.round(clipR)})`);
      }
    }
    return [...new Set(out)];
  }, dialog);
}

const findings: Record<string, string[]> = {};
function note(where: string, what: string[]) {
  if (what.length) (findings[where] ??= []).push(...what);
}

async function shoot(page: Page, file: string) {
  if (!SHOTS) return;
  mkdirSync(DIR, { recursive: true });
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(DIR, `${TAG}-${file}.png`) });
}

async function theme(page: Page, light: boolean) {
  await page.evaluate((l) => {
    if (l) document.documentElement.dataset.theme = "light";
    else delete document.documentElement.dataset.theme;
    dispatchEvent(new Event("gmclip-theme"));
  }, light);
}

/** The control is reachable: scrolled into its panel, then inside the window. */
async function reachable(page: Page, where: string, what: string, loc: Locator): Promise<string[]> {
  // (a panel reads its data as it opens: give it a moment)
  await loc.first().waitFor({ state: "visible", timeout: 8_000 }).catch(() => undefined);
  if (!(await loc.count())) return [`${what}: not there`];
  const l = loc.first();
  if (!(await l.isVisible())) return [`${what}: not visible`];
  await l.scrollIntoViewIfNeeded().catch(() => undefined);
  const b = await l.boundingBox();
  const vp = page.viewportSize()!;
  if (!b) return [`${what}: not laid out`];
  const bad: string[] = [];
  if (b.x < -1 || b.x + b.width > vp.width + 1) bad.push(`${what}: ${Math.round(b.x)}–${Math.round(b.x + b.width)} sideways in ${vp.width}`);
  if (b.y < -1 || b.y + b.height > vp.height + 1) bad.push(`${what}: ${Math.round(b.y)}–${Math.round(b.y + b.height)} down in ${vp.height}`);
  if (b.width < 4) bad.push(`${what}: ${b.width} px wide`);
  if (b.height < 4) bad.push(`${what}: ${b.height} px tall`);
  return bad.map((x) => `${where}: ${x}`);
}

interface Check {
  name: string;
  /** Shows the panel in the current window (idempotent). */
  open: () => Promise<void>;
  root: () => Locator;
  dialog?: boolean;
  primaries: [string, () => Locator][];
  /** Extra screenshots: a section scrolled into view first. */
  sections?: [string, () => Locator][];
}

const SHOT_SIZES = ["1440x900", "480x360", "500x1000"];

async function check(page: Page, c: Check) {
  for (const vp of SIZES) {
    await page.setViewportSize(vp);
    await page.waitForTimeout(150);
    const where = `${c.name} ${name(vp)}`;
    if (BEFORE) {
      const failed = await c.open().then(() => null, (e: unknown) => String(e).split("\n")[0]);
      if (failed) {
        note(where, [`${where}: could not open it (${failed})`]);
        if (SHOT_SIZES.includes(name(vp))) await shoot(page, `${c.name}-${name(vp)}`);
        await page.keyboard.press("Escape");
        continue;
      }
    } else {
      await c.open();
    }
    if (BEFORE) {
      // before this pass some panels could not be reached at all in a small window
      const shown = await c.root().first().waitFor({ state: "visible", timeout: 8_000 }).then(() => true, () => false);
      if (!shown) {
        note(where, [`${where}: cannot be shown in this window`]);
        if (SHOT_SIZES.includes(name(vp))) await shoot(page, `${c.name}-${name(vp)}`);
        continue;
      }
    }
    await expect(c.root().first(), `${where}: shows`).toBeVisible({ timeout: 20_000 });
    await page.waitForTimeout(300); // a tab bar or a sheet settles its layout after a resize
    const bad = await audit(c.root().first(), !!c.dialog);
    for (const [what, loc] of c.primaries) bad.push(...(await reachable(page, where, what, loc())));
    note(where, bad.map((b) => (b.startsWith(where) ? b : `${where}: ${b}`)));
    if (!BEFORE) expect.soft(bad, `${where}`).toEqual([]);
    if (SHOTS && SHOT_SIZES.includes(name(vp))) {
      if (c.sections?.length) {
        for (const [sec, loc] of c.sections) {
          if (await loc().count()) await loc().first().scrollIntoViewIfNeeded().catch(() => undefined);
          await shoot(page, `${c.name}-${sec}-${name(vp)}`);
        }
      } else {
        await shoot(page, `${c.name}-${name(vp)}`);
      }
      if (!BEFORE && name(vp) === "1440x900") {
        await theme(page, true);
        if (c.sections?.length) {
          for (const [sec, loc] of c.sections) {
            if (await loc().count()) await loc().first().scrollIntoViewIfNeeded().catch(() => undefined);
            await shoot(page, `${c.name}-${sec}-light-${name(vp)}`);
          }
        } else {
          await shoot(page, `${c.name}-light-${name(vp)}`);
        }
        await theme(page, false);
      }
    }
  }
  await page.setViewportSize({ width: 1440, height: 900 });
}

/** The View menu's item, when there is one (the menu is closed again otherwise). */
async function viewMenuItem(page: Page, testid: string): Promise<Locator | null> {
  const trigger = page.getByTestId("view-menu");
  if (!(await trigger.isVisible())) return null;
  if ((await trigger.getAttribute("aria-expanded")) !== "true") await trigger.click();
  const item = page.getByTestId(testid);
  if (await item.count()) return item;
  await page.keyboard.press("Escape");
  return null;
}

/** The tabs by their names before decision 0054 (Short is Captions, the Inspector is Look…). */
const RENAMED: Record<string, [string, string]> = { short: ["captions", "Captions"], inspector: ["look", "Look"], layout: ["look", "Look"], transcript: ["words", "Words"], tighten: ["words", "Words"] };

/** A side-panel tab, the way a person finds it: the View menu's item for it (after),
 * else the side panel shown and the tab picked (⌘K's "More panels" for one off the bar). */
async function sideTab(page: Page, was: string, wasLabel: string) {
  const [id, label] = RENAMED[was] ?? [was, wasLabel];
  const tab = page.getByRole("tab", { name: label, exact: true });
  const side = page.getByRole("complementary", { name: "Side panel" });
  // (attributes only of what is there: a missing element would be waited for)
  if ((await side.isVisible()) && (await tab.count()) && (await tab.first().getAttribute("aria-selected")) === "true") return;
  const item = await viewMenuItem(page, `view-tab-${id}`);
  if (item) {
    await item.click();
  } else {
    if (!(await side.isVisible())) {
      const show = await viewMenuItem(page, "view-side");
      await show?.click();
    }
    // the bars re-measure as they change: try again until the tab is the selected one
    await expect(async () => {
      await openTab(page, label);
    }).toPass({ timeout: 30_000 });
  }
  await expect(page.getByRole("tab", { name: label, exact: true })).toHaveAttribute("aria-selected", "true");
}

/** The project map, shown (docked or as a drawer). */
async function showMap(page: Page) {
  const map = page.getByTestId("project-map");
  if (await map.isVisible()) return;
  const item = await viewMenuItem(page, "view-map");
  await item?.click();
  await expect(map).toBeVisible();
}

/** Esc closes a sheet; before this pass some only closed from their own button. */
async function closeSheet(page: Page, sheet: Locator, button = "Close") {
  await page.keyboard.press("Escape");
  if (await sheet.isVisible().catch(() => false)) {
    note("escape", [`${button}: Esc did not close it`]);
    await sheet.getByRole("button", { name: button }).first().click();
  }
  await expect(sheet).toHaveCount(0);
}

async function palette(page: Page, what: string) {
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill(what);
  await page.keyboard.press("Enter");
}

/** Opens a video from Home and waits until its transcript is stored; its project id. */
async function openVideo(page: Page, path: string, title: string): Promise<string> {
  await page.getByLabel("Video path").fill(path);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText(title.slice(0, 8), { timeout: 30_000 });
  let pid = "";
  await expect
    .poll(async () => {
      pid = (await bridge<{ id: string; name: string }[]>(page, "project_list_open", {})).find((p) => p.name.includes(title))?.id ?? "";
      if (!pid) return false;
      const st = await bridge<{ sources: { transcript: boolean }[] }>(page, "analysis_status", {}, pid);
      return st.sources.length > 0 && st.sources.every((x) => x.transcript);
    }, { timeout: 300_000, intervals: [2_000] })
    .toBe(true);
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 60_000 });
  return pid;
}

// After each test (a failed test restarts the worker, and its module state with it): what
// it found, merged into the run's findings file.
test.afterEach(() => {
  mkdirSync(DIR, { recursive: true });
  const file = join(DIR, `${TAG}-findings.json`);
  const was = existsSync(file) ? (JSON.parse(readFileSync(file, "utf8")) as Record<string, string[]>) : {};
  writeFileSync(file, JSON.stringify({ ...was, ...findings }, null, 1));
  for (const k of Object.keys(findings)) delete findings[k];
});

test("every new panel at every window size", async ({ browser }) => {
  test.setTimeout(3_600_000);
  const { page, errors } = await openPage(browser);

  // another recording first (B-roll and topic search look across the library)
  const other = join(mkdtempSync(join(tmpdir(), "gmclip-ux-")), "last-week-stream.mp4");
  copyFileSync(VIDEO, other);
  await openVideo(page, other, "last-week-stream");
  await page.getByRole("button", { name: "Home", exact: true }).click();
  const pid = await openVideo(page, VIDEO, "own-vm");

  // three topics, a long-form of them and a fork in the other order
  const stream = (await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid)).cut;
  const tv = await bridge<{ start_ms: number[] }>(page, "transcript_view", { cut: stream }, pid);
  const at = (sec: number) => tv.start_ms.findIndex((x) => x >= sec * 1000);
  const topics: string[] = [];
  for (const [a, b, title] of [[0, 12, "Why a VM"], [15, 28, "Hosting costs"], [32, 45, "Hardware"]] as const) {
    topics.push((await bridge<{ topic: string }>(page, "topic_label_ord", { cut: stream, from: at(a), to: at(b) - 1, title }, pid)).topic);
  }
  const longA = (await bridge<{ cut: string }>(page, "longform_from_topics", { topics, order: "source", title: "Why every project gets a VM" }, pid)).cut;
  await bridge(page, "longform_fork", { cut: longA, topics: [...topics].reverse(), title: "Hardware first" }, pid);
  // a short from a tapped moment, its hook split off, and an alternate opening
  const m1 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 30, out_frame: 300 }, pid);
  const m2 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 420, out_frame: 720 }, pid);
  const short = (await bridge<{ cut: string }>(page, "cut_from_moment", { moment: m1.moment.id }, pid)).cut;
  await bridge(page, "cut_split", { cut: short, frame: 90 }, pid);
  const parts = (await bridge<{ parts: { id: string }[] }>(page, "cut_view", { cut: short }, pid)).parts;
  await bridge(page, "part_set_role", { part: parts[1]!.id, role: "other" }, pid);
  await bridge(page, "opening_add", { cut: short, from: { moment: m2.moment.id } }, pid);
  // title and description drafts, as an agent would write them
  await bridge(page, "copy_drafts_add", {
    cut: short,
    drafts: [
      { platform: "youtube", title: "Why I give every project its own VM", description: "One VM per project: skills, costs and the hardware it runs on." },
      { platform: "tiktok", title: "Every project gets its own VM", description: "Here is why. #devtools" },
    ],
  }, pid).catch(() => undefined);
  // an export of the short (the export popover's file, Home's newest exports)
  await bridge(page, "export_start", { cut: short }, pid);
  await expect.poll(async () => (await bridge<unknown[]>(page, "exports_list", {}, pid)).length, { timeout: 300_000, intervals: [2_000] }).toBeGreaterThan(0);

  // the short, from the map
  await showMap(page);
  await expect(page.getByTestId("map-short").first()).toBeVisible({ timeout: 20_000 });
  await page.getByTestId("map-short").first().locator(".map-open").click();
  await expect.poll(async () => (await bridge<{ cut: string }>(page, "viewer_state", {})).cut).toBe(short);
  const side = () => page.getByRole("complementary", { name: "Side panel" });

  // ---- the Short tab: hook lines, openings, end card
  await check(page, {
    name: "short-tab",
    open: async () => {
      await sideTab(page, "short", "Short");
      await rowOn(page, "openings");
      await rowOn(page, "hook");
    },
    root: side,
    primaries: [
      ["Add opening", () => page.getByTestId("opening-add")],
      ["Compare openings", () => page.getByTestId("openings-compare")],
      ["End card", () => page.getByTestId("endcard-lf").getByRole("heading")],
    ],
    sections: [
      ["hooklines", () => page.getByTestId("hook-lines")],
      ["openings", () => page.getByTestId("openings")],
      ["endcard", () => page.getByTestId("endcard-lf")],
    ],
  });

  // ---- the openings compare sheet
  await sideTab(page, "short", "Short");
  await page.getByTestId("openings-compare").scrollIntoViewIfNeeded();
  await expect(page.getByTestId("openings-compare")).toBeEnabled({ timeout: 20_000 });
  await page.getByTestId("openings-compare").click();
  await check(page, {
    name: "openings-sheet",
    dialog: true,
    open: async () => {
      if (!(await page.getByTestId("openings-sheet").isVisible())) {
        await sideTab(page, "short", "Short");
        await page.getByTestId("openings-compare").scrollIntoViewIfNeeded();
        await page.getByTestId("openings-compare").click();
      }
    },
    root: () => page.getByTestId("openings-sheet"),
    primaries: [
      ["Play", () => page.getByTestId("opening-play")],
      ["Make main", () => page.getByTestId("opening-choose")],
      ["Close", () => page.getByTestId("openings-sheet").getByRole("button", { name: "Close" })],
    ],
  });
  await closeSheet(page, page.getByTestId("openings-sheet"));

  // ---- the Thumbnail tab's titles and descriptions
  await check(page, {
    name: "thumbnail-drafts",
    open: () => sideTab(page, "thumbnail", "Thumbnail"),
    root: side,
    primaries: [["Draft with my agent", () => page.getByTestId("copy-agent")]],
    sections: [["drafts", () => page.getByTestId("copy-drafts")]],
  });

  // ---- the Polish tab
  await check(page, {
    name: "polish",
    open: () => sideTab(page, "polish", "Polish"),
    root: side,
    primaries: [
      ["Star stressed words", () => page.getByTestId("polish-auto")],
      ["Find screen jumps", () => page.getByTestId("polish-holds-find")],
      ["Loop", () => page.getByTestId("polish-loop-on")],
    ],
  });

  // ---- the Inspector: speed and ramp, plate, backdrop, video, covers and Pin to motion
  await sideTab(page, "inspector", "Inspector");
  await page.getByTestId("cover-add").scrollIntoViewIfNeeded();
  await page.getByTestId("cover-add").click();
  await expect(page.getByTestId("cover-row")).toHaveCount(1, { timeout: 20_000 });
  await check(page, {
    name: "inspector",
    open: () => sideTab(page, "inspector", "Inspector"),
    root: side,
    primaries: [
      ["Speed", () => page.getByTestId("part-speed")],
      ["Ramp from here", () => page.getByTestId("ramp-add")],
      ["Save plate", () => page.getByTestId("plate-save")],
      ["Backdrop", () => page.getByTestId("backdrop-set")],
      ["Video", () => page.getByTestId("video-overlay-add")],
      ["Pin to motion", () => page.getByTestId("cover-pin")],
    ],
    sections: [
      ["speed", () => page.getByTestId("ramp-add")],
      ["looks", () => page.getByTestId("plate-row")],
    ],
  });

  // ---- the export popover's 9:16 + 16:9 row, and its sheet
  await check(page, {
    name: "export-popover",
    open: async () => {
      const opts = page.getByRole("button", { name: "Export options" });
      if ((await opts.getAttribute("aria-expanded")) !== "true") await opts.click();
    },
    root: () => page.getByRole("group", { name: "Export options" }),
    dialog: true,
    primaries: [["Export 9:16 + 16:9", () => page.getByTestId("export-both")]],
  });
  await page.getByTestId("framings-open").click();
  await check(page, {
    name: "framings-sheet",
    dialog: true,
    open: async () => {
      if (!(await page.getByTestId("framings-sheet").isVisible())) {
        const opts = page.getByRole("button", { name: "Export options" });
        if ((await opts.getAttribute("aria-expanded")) !== "true") await opts.click();
        await page.getByTestId("framings-open").click();
      }
    },
    root: () => page.getByTestId("framings-sheet"),
    primaries: [["Export", () => page.getByTestId("framings-start")]],
  });
  await closeSheet(page, page.getByTestId("framings-sheet"));

  // ---- History: the version slider
  await check(page, {
    name: "history",
    dialog: true,
    open: async () => {
      if (!(await page.getByRole("dialog", { name: "History" }).isVisible())) await palette(page, "History");
    },
    root: () => page.getByRole("dialog", { name: "History" }),
    primaries: [["Version slider", () => page.getByTestId("history-slider")]],
  });
  await closeSheet(page, page.getByRole("dialog", { name: "History" }));

  // ---- the keys list
  await check(page, {
    name: "keys",
    dialog: true,
    open: async () => {
      if (!(await page.getByRole("dialog", { name: "Keyboard shortcuts" }).isVisible())) {
        await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }).catch(() => undefined);
        await page.keyboard.press("?");
      }
    },
    root: () => page.getByRole("dialog", { name: "Keyboard shortcuts" }),
    primaries: [["Close", () => page.getByRole("dialog", { name: "Keyboard shortcuts" }).getByRole("button", { name: "Close" })]],
  });
  await page.keyboard.press("Escape");

  // ---- B-roll for a few words (the words strip)
  const strip = page.getByRole("listbox", { name: "Cut words" });
  await check(page, {
    name: "broll",
    open: async () => {
      if (await page.getByTestId("broll-pop").isVisible()) return;
      if (await strip.isVisible()) {
        // the words strip's selection
        const live = strip.locator(".w:not(.struck)");
        await live.nth(4).click();
        await live.nth(9).click({ modifiers: ["Shift"] });
        await page.getByTestId("broll-open").click();
      } else {
        // a small window has no words strip: the transcript's selection
        await sideTab(page, "transcript", "Transcript");
        const ws = page.getByTestId("transcript").locator(".w");
        await ws.nth(4).click();
        await ws.nth(9).click({ modifiers: ["Shift"] });
        const b = page.getByTestId("tr-broll");
        if (!(await b.count())) return;
        await b.click();
      }
      await expect(page.getByTestId("broll-pop").getByTestId("broll-busy")).toHaveCount(0, { timeout: 90_000 });
    },
    root: () => page.getByTestId("broll-pop"),
    primaries: [],
  });
  await page.keyboard.press("Escape");

  // ---- the long-form: chapters bar, fork and compare on the map
  await page.setViewportSize({ width: 1440, height: 900 });
  await showMap(page);
  await page.getByTestId("map-long").filter({ hasText: "Why every project" }).locator(".map-open").click();
  await expect.poll(async () => (await bridge<{ cut: string }>(page, "viewer_state", {})).cut).toBe(longA);
  await check(page, {
    name: "chapters-bar",
    open: async () => undefined,
    root: () => page.getByTestId("chapters-bar"),
    primaries: [
      ["Copy chapters", () => page.getByTestId("chapters-copy")],
      ["Show chapters", () => page.getByTestId("chapters-show")],
    ],
  });
  await check(page, {
    name: "map-longforms",
    open: () => showMap(page),
    root: () => page.getByTestId("project-map"),
    primaries: [
      ["Fork", () => page.getByTestId("longform-fork")],
      ["Compare long-forms", () => page.getByTestId("longform-compare-open")],
      ["Draw layout", () => page.getByTestId("draw-layout-open")],
    ],
  });
  await showMap(page);
  await page.getByTestId("longform-compare-open").click();
  await check(page, {
    name: "longform-compare",
    dialog: true,
    open: async () => {
      if (!(await page.getByTestId("longform-compare").isVisible())) {
        await showMap(page);
        await page.getByTestId("longform-compare-open").click();
      }
    },
    root: () => page.getByTestId("longform-compare"),
    primaries: [
      ["Pick this one", () => page.getByTestId("lc-b-pick")],
      ["Keep both", () => page.getByTestId("lc-keep-both")],
    ],
  });
  await closeSheet(page, page.getByTestId("longform-compare"), "Keep both");

  // ---- Draw your layout
  await showMap(page);
  await page.getByTestId("draw-layout-open").first().click();
  await check(page, {
    name: "draw-layout",
    dialog: true,
    open: async () => {
      if (!(await page.getByTestId("draw-layout").isVisible())) {
        await showMap(page);
        await page.getByTestId("draw-layout-open").first().click();
      }
    },
    root: () => page.getByTestId("draw-layout"),
    primaries: [
      ["The picture", () => page.getByTestId("dl-stage")],
      ["Save layout", () => page.getByTestId("dl-save")],
      ["Different frame", () => page.getByTestId("dl-frame")],
    ],
  });
  await closeSheet(page, page.getByTestId("draw-layout"), "Skip");

  // ---- library search: topic results
  await page.keyboard.press("ControlOrMeta+Shift+f");
  await page.getByTestId("library-search-input").fill("hosting");
  await expect(page.getByTestId("library-topic").first()).toBeVisible({ timeout: 60_000 });
  await check(page, {
    name: "search-topics",
    open: async () => undefined,
    root: () => page.getByRole("main", { name: "Search the library" }),
    primaries: [["Open the topic", () => page.getByTestId("topic-open")]],
  });

  // ---- the library-wide Home
  await page.getByRole("button", { name: "Home", exact: true }).click();
  await check(page, {
    name: "home",
    open: async () => undefined,
    root: () => page.getByRole("main", { name: "Home" }),
    primaries: [
      ["Search", () => page.getByTestId("home-search")],
      ["Video path", () => page.getByLabel("Video path")],
    ],
  });

  expect(errors).toEqual([]);
});

test("every new panel is in the View menu or ⌘K", async ({ browser }) => {
  test.skip(BEFORE, "after only");
  test.setTimeout(600_000);
  const { page, errors } = await openPage(browser);
  await openVideo(page, VIDEO, "own-vm");
  // the View menu: the bar's tabs (decision 0054: Words, Look, Captions)
  for (const [id, label] of [["words", "Words"], ["look", "Look"], ["captions", "Captions"]] as const) {
    for (const vp of [{ width: 1440, height: 900 }, { width: 480, height: 360 }]) {
      await page.setViewportSize(vp);
      const item = await viewMenuItem(page, `view-tab-${id}`);
      expect(item, `View › ${label} at ${name(vp)}`).not.toBeNull();
      await item!.click();
      await expect(page.getByRole("tab", { name: label, exact: true }), `${label} at ${name(vp)}`).toHaveAttribute("aria-selected", "true");
      await expect(page.getByRole("complementary", { name: "Side panel" })).toBeVisible();
    }
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  // ⌘K's "More panels": the panels off the bar, each opened (and in the bar while open)
  for (const label of ["Story", "Polish", "Thumbnail", "Review", "Agent"]) {
    await page.keyboard.press("ControlOrMeta+k");
    await page.getByTestId("palette-input").fill("more panels");
    const item = page.getByTestId("palette-item").filter({ hasText: `${label} panel` });
    await expect(item.getByTestId("palette-section"), `⌘K › More panels › ${label}`).toHaveText("More panels");
    await item.click();
    await expect(page.getByRole("tab", { name: label, exact: true })).toHaveAttribute("aria-selected", "true");
  }
  // Tighten is in the status dropdown (2026-11-22: one Tighten)
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("tighten");
  await page.getByTestId("palette-item").filter({ hasText: "Tighten: pauses, fillers and retakes" }).click();
  await expect(page.getByTestId("checks")).toHaveAttribute("open", "");
  await expect(page.getByTestId("words-tighten")).toHaveAttribute("open", "");
  await page.getByTestId("checks").locator("summary").first().click();
  // ⌘K: each new feature by the words a person would type
  for (const [q, want] of [
    ["openings", "Compare openings"],
    ["hook lines", "Hook lines"],
    ["end card", "End card to the long-form"],
    ["titles", "Titles and descriptions"],
    ["polish", "Polish"],
    ["speed", "Speed and ramp"],
    ["backdrop", "Backdrop"],
    ["draw layout", "Draw your layout"],
    ["compare long", "Compare long-forms"],
    ["b-roll", "B-roll"],
    ["9:16 16:9", "Export 9:16 and 16:9"],
    ["history", "History"],
    ["keyboard", "Keyboard shortcuts"],
  ] as const) {
    await page.keyboard.press("ControlOrMeta+k");
    await page.getByTestId("palette-input").fill(q);
    await expect(page.getByTestId("palette-item").filter({ hasText: want }).first(), `⌘K “${q}”`).toBeVisible();
    await page.keyboard.press("Escape");
  }
  // the ? list names ⌘K
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toContainText("Command palette");
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});

test("Proxy building… fits a small window", async ({ browser }) => {
  test.skip(!existsSync(LONG), "no long recording on this machine");
  test.setTimeout(300_000);
  const { page } = await openPage(browser);
  await page.getByLabel("Video path").fill(LONG);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("loc_684c44b6.proxy", { timeout: 30_000 });
  const chip = page.getByTestId("proxy-building");
  await expect(chip).toBeVisible({ timeout: 30_000 });
  await check(page, {
    name: "proxy-note",
    open: async () => {
      if (!(await page.locator(".timeline").isVisible())) await (await viewMenuItem(page, "view-timeline"))?.click();
    },
    root: () => page.getByRole("toolbar", { name: "Timeline tools" }),
    primaries: [["Proxy building", () => chip]],
  });
});
