// U.17 accessibility, pending axe (a new dev dependency: AJ.03's supply-chain review): the
// checks axe would call serious, done by hand on Home, the cut view's tabs and the board —
// every button, link, input, select and textarea has an accessible name, every image has
// alt text, no id repeats, and every dialog is labelled. The panels and sheets added on
// 2026-10-16/17 (Polish, the View menu's tabs, export popover and its 9:16 + 16:9 sheet,
// History, the keys list, openings compare, long-form compare, chapters, Draw your layout
// and search) too, and each sheet is modal: it takes the focus, Tab stays inside,
// Esc closes it and gives the focus back.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
const WIDE = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

async function audit(page: Page, where: string): Promise<string[]> {
  return page.evaluate((where) => {
    const out: string[] = [];
    const visible = (e: Element) => {
      const r = (e as HTMLElement).getBoundingClientRect();
      const s = getComputedStyle(e as HTMLElement);
      return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none";
    };
    const name = (e: Element): string => {
      const h = e as HTMLElement;
      const aria = h.getAttribute("aria-label");
      if (aria?.trim()) return aria;
      const by = h.getAttribute("aria-labelledby");
      if (by) return by.split(/\s+/).map((id) => document.getElementById(id)?.textContent ?? "").join(" ");
      if ((h as HTMLInputElement).labels?.length) return [...(h as HTMLInputElement).labels!].map((l) => l.textContent ?? "").join(" ");
      const t = h.textContent?.trim();
      if (t && !["INPUT", "SELECT", "TEXTAREA"].includes(h.tagName)) return t;
      return h.getAttribute("title") ?? h.getAttribute("placeholder") ?? (h.tagName === "SELECT" ? "" : "");
    };
    for (const e of document.querySelectorAll("button, a[href], input:not([type=hidden]), select, textarea, [role=button], [role=tab]")) {
      if (!visible(e)) continue;
      if (!name(e).trim()) out.push(`${where}: unnamed ${e.tagName.toLowerCase()} class="${(e as HTMLElement).className}" testid="${(e as HTMLElement).dataset.testid ?? ""}"`);
    }
    for (const img of document.querySelectorAll("img")) if (!img.hasAttribute("alt")) out.push(`${where}: img without alt`);
    const ids = new Map<string, number>();
    for (const e of document.querySelectorAll("[id]")) ids.set(e.id, (ids.get(e.id) ?? 0) + 1);
    for (const [id, n] of ids) if (n > 1) out.push(`${where}: id "${id}" ×${n}`);
    for (const d of document.querySelectorAll("[role=dialog]")) if (!name(d).trim()) out.push(`${where}: unlabelled dialog`);
    return out;
  }, where);
}

test("accessible names everywhere the person works", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  const problems: string[] = [];
  await expect(page.getByRole("region", { name: "Open a video" })).toBeVisible();
  problems.push(...(await audit(page, "home")));
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  for (const tab of ["Inspector", "Layout", "Short", "Polish", "Story", "Tighten", "Agent", "Review", "Thumbnail"]) {
    await openTab(page, tab);
    problems.push(...(await audit(page, `cut/${tab}`)));
  }
  // the export popover and the status pill (the board folded in, decision 0054)
  await page.getByRole("button", { name: "Export options" }).click();
  problems.push(...(await audit(page, "cut/export options")));
  await page.getByRole("button", { name: "Export options" }).click();
  await page.getByTestId("checks").locator("summary").click();
  problems.push(...(await audit(page, "cut/status")));
  console.log(problems.join("\n"));
  expect(problems).toEqual([]);
});

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

/** A modal sheet: labelled, aria-modal, holding the focus; Tab never leaves it; Esc closes
 *  it and the focus goes back to `opener`. */
async function modal(page: Page, where: string, sheet: ReturnType<Page["locator"]>, opener: ReturnType<Page["locator"]> | null, problems: string[]) {
  await expect(sheet).toBeVisible();
  await expect(sheet, `${where}: modal`).toHaveAttribute("aria-modal", "true");
  await expect.poll(() => sheet.evaluate((d) => d.contains(document.activeElement)), { message: `${where}: takes the focus` }).toBe(true);
  for (let i = 0; i < 25; i++) {
    await page.keyboard.press("Tab");
    if (!(await sheet.evaluate((d) => d.contains(document.activeElement)))) {
      problems.push(`${where}: Tab left the sheet for ${await page.evaluate(() => { const a = document.activeElement as HTMLElement | null; return a ? `${a.tagName} ${a.className} ${(a.getAttribute("aria-label") ?? a.textContent ?? "").slice(0, 40)}` : "nothing"; })}`);
      break;
    }
  }
  problems.push(...(await audit(page, where)));
  await page.keyboard.press("Escape");
  await expect(sheet, `${where}: Esc closes it`).toHaveCount(0);
  if (opener) await expect.poll(() => opener.evaluate((o) => o === document.activeElement || o.contains(document.activeElement)), { message: `${where}: focus back on the opener` }).toBe(true);
}

test("the new panels and sheets: names, modal sheets, keyboard", async ({ browser }) => {
  test.skip(!existsSync(WIDE), "no wide test video on this machine");
  test.setTimeout(900_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  const problems: string[] = [];
  await page.getByLabel("Video path").fill(WIDE);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 600_000 });
  const pid = (await bridge<{ id: string; name: string }[]>(page, "project_list_open", {})).find((p) => p.name.includes("own-vm"))!.id;
  const stream = (await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid)).cut;
  const tv = await bridge<{ start_ms: number[] }>(page, "transcript_view", { cut: stream }, pid);
  const at = (sec: number) => tv.start_ms.findIndex((x) => x >= sec * 1000);
  const topics: string[] = [];
  for (const [a, b, title] of [[0, 12, "Why a VM"], [15, 28, "Skills"], [32, 45, "Hardware"]] as const) {
    topics.push((await bridge<{ topic: string }>(page, "topic_label_ord", { cut: stream, from: at(a), to: at(b) - 1, title }, pid)).topic);
  }
  const long = (await bridge<{ cut: string }>(page, "longform_from_topics", { topics, order: "source", title: "Long A" }, pid)).cut;
  await bridge(page, "longform_fork", { cut: long, topics: [...topics].reverse(), title: "Long B" }, pid);
  const m1 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 30, out_frame: 300 }, pid);
  const m2 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream, in_frame: 420, out_frame: 720 }, pid);
  const short = (await bridge<{ cut: string }>(page, "cut_from_moment", { moment: m1.moment.id }, pid)).cut;
  await bridge(page, "cut_split", { cut: short, frame: 90 }, pid);
  await bridge(page, "opening_add", { cut: short, from: { moment: m2.moment.id } }, pid);

  // the View menu: every pane and every side-panel tab, named
  await page.getByTestId("view-menu").click();
  await expect(page.getByRole("menu", { name: "View" })).toBeVisible();
  problems.push(...(await audit(page, "view menu")));
  await page.keyboard.press("Escape");

  // the short: Short (openings) and Polish, then the openings compare sheet
  const map = page.getByTestId("project-map");
  await expect(map.getByTestId("map-short").first()).toBeVisible({ timeout: 20_000 });
  await map.getByTestId("map-short").first().locator(".map-open").click();
  await expect.poll(async () => (await bridge<{ cut: string }>(page, "viewer_state", {})).cut).toBe(short);
  await openTab(page, "Polish");
  problems.push(...(await audit(page, "cut/Polish")));
  await openTab(page, "Short");
  const compare = page.getByTestId("openings-compare");
  await compare.scrollIntoViewIfNeeded();
  await expect(compare).toBeEnabled({ timeout: 20_000 });
  await compare.focus();
  await page.keyboard.press("Enter");
  await modal(page, "openings sheet", page.getByTestId("openings-sheet"), compare, problems);

  // the export popover and its 9:16 + 16:9 sheet
  await page.getByRole("button", { name: "Export options" }).click();
  problems.push(...(await audit(page, "export options")));
  await page.getByTestId("framings-open").click();
  await modal(page, "framings sheet", page.getByTestId("framings-sheet"), null, problems);

  // History (the version slider) and the keys list
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("History");
  problems.push(...(await audit(page, "palette")));
  await page.keyboard.press("Enter");
  const hist = page.getByRole("dialog", { name: "History" });
  await expect(hist).toBeVisible();
  problems.push(...(await audit(page, "history")));
  await page.keyboard.press("Escape");
  await expect(hist).toHaveCount(0);

  // the long-form: chapters bar, fork, compare sheet, the long-form sheet, Draw your layout
  await map.getByTestId("map-long").filter({ hasText: "Long A" }).locator(".map-open").click();
  await expect(page.getByTestId("chapters-bar")).toBeVisible({ timeout: 20_000 });
  problems.push(...(await audit(page, "long-form")));
  const cmp = map.getByTestId("longform-compare-open");
  await cmp.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("lc-summary")).toBeVisible({ timeout: 20_000 });
  await modal(page, "long-form compare", page.getByTestId("longform-compare"), cmp, problems);
  await map.getByTestId("map-stream").click();
  const lfNew = map.getByTestId("longform-new");
  await lfNew.focus();
  await page.keyboard.press("Enter");
  await modal(page, "long-form sheet", page.getByTestId("longform-sheet"), lfNew, problems);
  const draw = map.getByTestId("draw-layout-open").first();
  await draw.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("dl-stage")).toBeVisible({ timeout: 30_000 });
  await modal(page, "draw your layout", page.getByTestId("draw-layout"), draw, problems);

  // search (topic results)
  await page.keyboard.press("ControlOrMeta+Shift+f");
  await page.getByTestId("library-search-input").fill("hardware");
  await expect(page.getByTestId("library-topic").first()).toBeVisible({ timeout: 60_000 });
  problems.push(...(await audit(page, "search")));
  console.log(problems.join("\n"));
  expect(problems).toEqual([]);
  expect(errors).toEqual([]);
});
