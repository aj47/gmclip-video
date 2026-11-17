// Ideas 2026-10-16 (library and layout, K.02–K.06) on two fresh recordings: topics on the
// stream edit, a long-form from them, a fork, two long-forms compared (pick one, ⌘Z
// keeps both), library search by topic landing in the long-form, "Fix everywhere" from a
// caption word, B-roll from the other recording, and laughs/reactions found as moments
// (reactions_find). Uses the lab media folder (tests/e2e/lab.ts):
// the second recording is a copy of the first under another name.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no lab media on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

interface Open {
  id: string;
  name: string;
}

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

/** Opens a video from Home and waits until its transcript is stored; its project id. */
async function openVideo(page: Page, path: string, name: string): Promise<string> {
  await page.getByLabel("Video path").fill(path);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText(name.slice(0, 8), { timeout: 30_000 });
  let pid = "";
  await expect
    .poll(async () => {
      pid = (await bridge<Open[]>(page, "project_list_open", {})).find((p) => p.name.includes(name))?.id ?? "";
      if (!pid) return false;
      const st = await bridge<{ sources: { transcript: boolean }[] }>(page, "analysis_status", {}, pid);
      return st.sources.length > 0 && st.sources.every((x) => x.transcript);
    }, { timeout: 300_000, intervals: [2_000] })
    .toBe(true);
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 60_000 });
  return pid;
}


test("long-forms compared, topics searched, names fixed, B-roll and reactions", async ({ browser }) => {
  test.setTimeout(900_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");

  // another recording first (a copy under another name): the library has two streams
  const other = join(mkdtempSync(join(tmpdir(), "gmclip-ideas-")), "last-week-stream.mp4");
  copyFileSync(VIDEO, other);
  await openVideo(page, other, "last-week-stream");
  await page.getByRole("button", { name: "Home" }).click();
  const pid = await openVideo(page, VIDEO, "own-vm");

  // two topics on the stream edit
  await openTab(page, "Transcript"); // the side panel opens on Look
  const tr = page.getByTestId("transcript");
  const words = tr.locator(".w");
  await expect.poll(() => words.count(), { timeout: 30_000 }).toBeGreaterThan(80);
  await words.nth(0).click();
  await words.nth(20).click({ modifiers: ["Shift"] });
  await page.keyboard.press("t");
  await page.getByTestId("topic-title").fill("Why a VM");
  await page.getByTestId("topic-title").press("Enter");
  await words.nth(30).click();
  await words.nth(60).click({ modifiers: ["Shift"] });
  await page.keyboard.press("t");
  await page.getByTestId("topic-title").fill("Hosting costs");
  await page.getByTestId("topic-title").press("Enter");
  await expect(page.getByTestId("topic-head")).toHaveCount(2);

  // a long-form from both, then a fork of it (same order), then one in the other order
  const map = page.getByTestId("project-map");
  await map.getByTestId("longform-new").click();
  const lsheet = page.getByTestId("longform-sheet");
  await lsheet.getByRole("checkbox").nth(0).check();
  await lsheet.getByRole("checkbox").nth(1).check();
  await lsheet.getByTestId("longform-make").click();
  await expect(page.getByTestId("cut-title")).toHaveText("Why a VM", { timeout: 20_000 });
  await expect(map.getByTestId("map-long")).toHaveCount(1);
  await map.getByTestId("map-long").getByTestId("longform-fork").click();
  await expect(page.getByTestId("cut-title")).toHaveText("Why a VM (fork)", { timeout: 20_000 });
  await expect(map.getByTestId("map-long")).toHaveCount(2);
  const m = await bridge<{ longs: { cut: string; title: string; topics: string[] }[] }>(page, "project_map_view", {}, pid);
  const orig = m.longs.find((l) => l.title === "Why a VM")!;
  const swapped = await bridge<{ cut: string }>(page, "longform_fork", { cut: orig.cut, topics: [...orig.topics].reverse(), title: "Costs first" }, pid);
  await map.getByTestId("map-long").first().getByRole("button").first().click();
  await expect(page.getByTestId("cut-title")).toHaveText("Why a VM", { timeout: 20_000 });

  // compare: the original and the other order side by side
  await expect(map.getByTestId("map-long")).toHaveCount(3);
  await map.getByTestId("longform-compare-open").click();
  const cmp = page.getByTestId("longform-compare");
  await expect(cmp).toBeVisible();
  await cmp.getByLabel("Second long-form").selectOption(swapped.cut);
  await expect(cmp.getByTestId("lc-summary")).toHaveText("The same topics; 2 in another place.");
  await expect(cmp.getByTestId("lc-a").getByTestId("lc-chapter").first()).toContainText("Why a VM");
  await expect(cmp.getByTestId("lc-b").getByTestId("lc-chapter").first()).toContainText("Hosting costs");
  await expect(cmp.locator(".lc-thumb canvas").first()).toBeVisible({ timeout: 20_000 });
  // pick the other order: the original goes; the toast's Undo keeps both
  await cmp.getByTestId("lc-b-pick").click();
  await expect(cmp).toHaveCount(0);
  await expect(map.getByTestId("map-long")).toHaveCount(2);
  await expect(page.getByTestId("cut-title")).toHaveText("Costs first", { timeout: 20_000 });
  await expect(page.locator(".toast")).toContainText("Kept one long-form");
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("Meta+z");
  await expect(map.getByTestId("map-long")).toHaveCount(3);

  // library search by topic: lands in the long-form that plays it
  await page.keyboard.press("Meta+Shift+F");
  await page.getByTestId("library-search-input").fill("hosting");
  const topic = page.getByTestId("library-topic").filter({ hasText: "Hosting costs" });
  await expect(topic).toBeVisible({ timeout: 30_000 });
  await expect(topic).toContainText("in “Why a VM”");
  await topic.getByTestId("topic-open").click();
  await expect(page.getByTestId("cut-title")).toHaveText("Why a VM", { timeout: 20_000 });
  // at the chapter, not the start
  await expect.poll(async () => (await page.getByTestId("timecode").textContent())?.startsWith("0:00.00"), { timeout: 20_000 }).toBe(false);

  // Fix everywhere from a caption word in the words strip
  const strip = page.getByRole("listbox", { name: "Cut words" });
  await expect(strip).toBeVisible({ timeout: 30_000 });
  const live = strip.locator(".w:not(.struck)");
  const w = live.nth(3);
  const heard = ((await w.textContent()) ?? "").replace(/[.,?!]+$/, "");
  await w.click();
  await page.getByTestId("fix-everywhere").click();
  await page.getByTestId("fix-written").fill("Gmclipped");
  await page.getByTestId("fix-all").check();
  await page.getByTestId("fix-apply").click();
  await expect(page.locator(".toast")).toContainText(`“${heard}” shows as “Gmclipped” in every caption`);
  await expect(page.locator(".toast")).toContainText("all your projects");
  await expect(strip.locator(".w.edited").filter({ hasText: "Gmclipped" }).first()).toBeVisible({ timeout: 20_000 });
  const terms = await bridge<{ terms: { written: string; heard_as: string[]; profile: boolean }[] }>(page, "terms_view", {}, pid);
  expect(terms.terms.find((t) => t.written === "Gmclipped")?.profile).toBe(true);
  await page.keyboard.press("Meta+z");

  // B-roll for a few words: the other recording talks about the same thing
  const a = live.nth(10);
  const b = live.nth(16);
  await a.click();
  await b.click({ modifiers: ["Shift"] });
  const pop = page.getByTestId("broll-pop");
  await expect.poll(async () => {
    // (the meaning index may still be embedding the other recording: ask again)
    await page.getByTestId("broll-open").click();
    await expect(pop.getByTestId("broll-busy")).toHaveCount(0, { timeout: 90_000 });
    return pop.getByTestId("broll-row").count();
  }, { timeout: 240_000, intervals: [5_000] }).toBeGreaterThan(0);
  const row = pop.getByTestId("broll-row").filter({ hasText: "last-week-stream" }).first();
  await expect(row).toHaveAttribute("data-add", "image");
  await expect(row).toContainText("its frame goes over the selection");
  // decision 0028: it can also go in as a clip (its own video, silent, picture-in-picture)
  await expect(row.getByTestId("broll-add-clip")).toBeVisible();
  await row.getByTestId("broll-add").click();
  await expect(page.locator(".toast")).toContainText("B-roll frame added over the selection");

  // laughs and reactions as moments (found by sound and transcript), the way an agent asks
  const reactions = await bridge<{ found: number; added: number; candidates: unknown[] }>(page, "reactions_find", {}, pid);
  expect(reactions.added).toBeLessThanOrEqual(reactions.found);
  expect(Array.isArray(reactions.candidates)).toBe(true);
  expect(errors).toEqual([]);
});
