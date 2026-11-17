// Decision 0011 (SPEC §5.17) on a fresh video: the opened video is the project's stream
// edit; label topics from the transcript (drag/⇧-click + T, ⌥-click a line, ⇧T split,
// drag an edge), a long-form from a topic, "Stream edit changed — N
// edits" with diff and Pull, ⌘↑/⌘↓ between levels, a second framing, proposals from an
// agent (dashed, accepted here), a fluff proposal struck, and a struck run over 30 s
// collapsed.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");
/** The file a project is opened from: the app names it after the link's target. */
const REAL = existsSync(VIDEO) ? realpathSync(VIDEO) : VIDEO;

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

interface Overlay {
  topics: { id: string; title: string; state: string; w0: number; w1: number }[];
}

test("stream edit: topics, long-form, pull, levels, proposals", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");

  // The opened video is the stream edit; the map says so.
  const map = page.getByTestId("project-map");
  await expect(map.getByTestId("map-stream")).toBeVisible();
  await openTab(page, "Transcript"); // the side panel opens on Look
  const tr = page.getByTestId("transcript");
  const words = tr.locator(".w");
  await expect.poll(() => words.count(), { timeout: 30_000 }).toBeGreaterThan(80);
  const n = await words.count();

  // T labels a dragged/⇧-clicked selection; Enter applies the title.
  await words.nth(0).click();
  await words.nth(20).click({ modifiers: ["Shift"] });
  await page.keyboard.press("t");
  await page.getByTestId("topic-title").fill("Why a VM");
  await page.getByTestId("topic-title").press("Enter");
  const heads = page.getByTestId("topic-head");
  await expect(heads.filter({ hasText: "Why a VM" })).toHaveCount(1);
  await expect(map).toContainText("Why a VM");

  // ⌥-click selects a whole line; T labels it.
  const line = tr.locator(".tr-line").nth(4);
  const lineWords = await line.locator(".w").count();
  await line.locator(".w").nth(1).click({ modifiers: ["Alt"] });
  await expect(page.getByRole("toolbar", { name: "Transcript actions" })).toContainText(`${lineWords} words`);
  await page.keyboard.press("t");
  await page.getByTestId("topic-title").fill("Setup");
  await page.getByTestId("topic-title").press("Enter");
  await expect(heads).toHaveCount(2);

  // ⇧T splits the topic at the caret.
  await words.nth(10).click();
  await page.keyboard.press("Shift+T");
  await expect(heads).toHaveCount(3);

  // Drag the first topic's end onto word 5: it now ends there.
  const pid = await currentProject(page);
  const cut = await currentCut(page);
  expect(pid && cut).toBeTruthy();
  await heads.first().getByTestId("topic-edge-end").hover();
  await page.mouse.down();
  await words.nth(5).hover();
  await page.mouse.up();
  await expect.poll(async () => (await bridge<Overlay>(page, "stream_overlay", { cut }, pid)).topics.find((t) => t.title === "Why a VM")?.w1).toBe(5);

  // A long-form from "Setup".
  await map.getByTestId("longform-new").click();
  const sheet = page.getByTestId("longform-sheet");
  await sheet.getByRole("checkbox").nth(2).check();
  await expect(sheet).toContainText("1 topic");
  await sheet.getByTestId("longform-make").click();
  await expect(page.getByTestId("cut-title")).toHaveText("Setup", { timeout: 20_000 });
  await expect(map.getByTestId("map-long")).toHaveCount(1);
  const long = await currentCut(page);

  // ⌘↑ from a word of the long-form: the same word in the stream edit.
  const lwords = page.getByTestId("transcript").locator(".w:not(.out)");
  await expect.poll(() => lwords.count(), { timeout: 20_000 }).toBeGreaterThan(3);
  const ord = Number(await lwords.nth(2).getAttribute("data-i"));
  await lwords.nth(2).click();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+ArrowUp");
  await expect.poll(() => currentCut(page)).toBe(cut);
  await expect(tr.locator(`.w.sel[data-i="${ord}"]`)).toHaveCount(1, { timeout: 20_000 });
  // ⌘↓: the one cut using it opens
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+ArrowDown");
  await expect.poll(() => currentCut(page)).toBe(long);

  // The stream edit strikes a word inside "Setup": the long-form asks before taking it.
  await map.getByTestId("map-stream").click();
  await expect.poll(() => currentCut(page)).toBe(cut);
  await tr.locator(`.w[data-i="${ord + 1}"]`).click();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("d");
  await expect(tr.locator(`.w.out[data-i="${ord + 1}"]`)).toHaveCount(1);
  await expect(map.getByTestId("map-edits")).toHaveText("1 edits");
  await map.getByTestId("map-long").getByRole("button").first().click();
  await expect.poll(() => currentCut(page)).toBe(long);
  const banner = page.getByTestId("parent-banner");
  await expect(banner).toContainText("Stream edit changed — 1 edit");
  await banner.getByTestId("parent-diff").click();
  await expect(banner.getByRole("list", { name: "Stream edit changes" })).toContainText("struck");
  await banner.getByTestId("parent-pull").click();
  await expect(banner).toHaveCount(0);

  // A second framing of the long-form: its own crops, the same words.
  await map.getByTestId("framing-new").click();
  await map.getByLabel("Framing name").fill("face big");
  await map.getByTestId("framing-add").click();
  await expect(map.getByTestId("map-framings").getByRole("listitem")).toHaveCount(2);

  // An agent's proposals arrive dashed; the human accepts one and strikes the fluff.
  await map.getByTestId("map-stream").click();
  await expect.poll(() => currentCut(page)).toBe(cut);
  const tv = await bridge<{ revision: string; start_ms: number[]; dur_ms: number[]; text: string[] }>(page, "transcript_view", { cut }, pid);
  const wref = (i: number) => ({
    word_id: { asr_revision: tv.revision, ordinal: i },
    t_src_start: tv.start_ms[i]! * 705_600,
    t_src_end: (tv.start_ms[i]! + tv.dur_ms[i]!) * 705_600,
    normalised_text: (tv.text[i] ?? "").toLowerCase().replace(/[^a-z0-9]/g, ""),
  });
  const a0 = n - 30;
  await bridge(page, "dev_agent_call", { command: "topic_set", args: { start: wref(a0), end: wref(a0 + 12), title: "Agent's idea", pass_ref: "topics_fluff@1:e2e" } }, pid);
  await bridge(page, "dev_agent_call", { command: "fluff_propose", args: { items: [{ start: wref(a0 + 15), end: wref(a0 + 17), reason: "a stumble" }] } }, pid);
  const proposed = heads.filter({ hasText: "Agent's idea" });
  await expect(proposed).toHaveClass(/proposed/, { timeout: 20_000 });
  await proposed.getByTestId("topic-accept").click();
  await expect(proposed).not.toHaveClass(/proposed/);
  await page.getByTestId("fluff-head").getByTestId("fluff-accept").click();
  await expect(tr.locator(`.w.out[data-i="${a0 + 16}"]`)).toHaveCount(1);

  // A struck run over 30 s collapses to one row; a click shows it again.
  const ms = tv.start_ms;
  const from = ms.findIndex((x) => x > ms[0]! + 3_000);
  const to = ms.findIndex((x) => x > ms[from]! + 42_000);
  expect(from > 0 && to > from).toBeTruthy();
  await words.nth(from).click();
  await words.nth(to).click({ modifiers: ["Shift"] });
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("d");
  const run = page.getByTestId("struck-run");
  await expect(run.first()).toContainText("struck", { timeout: 20_000 });
  await run.first().click();
  await expect(run).toHaveCount(0);
  expect(errors).toEqual([]);
});

interface Open {
  id: string;
  name: string;
  cuts: { id: string; title: string }[];
}

/** The project opened from VIDEO (named after the file). */
async function currentProject(page: Page): Promise<string> {
  const list = await bridge<Open[]>(page, "project_list_open", {}, "");
  return list.find((p) => REAL.includes(p.name))?.id ?? "";
}

/** The cut on screen: the project's cut whose title the header shows. */
async function currentCut(page: Page): Promise<string> {
  const title = (await page.getByTestId("cut-title").textContent()) ?? "";
  const list = await bridge<Open[]>(page, "project_list_open", {}, "");
  const p = list.find((x) => REAL.includes(x.name));
  return p?.cuts.find((c) => c.title === title)?.id ?? "";
}
