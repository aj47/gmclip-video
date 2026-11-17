// U.07: the story panel on loc_4fadb756_h37 (its stream's project fixture): duration chip in
// the 9:16 band, brief autosaves, a typed out point, ‹ › nudges, ↑ ↓, ✕ with undo.
// A2.08 / U.07 on AJ's h79 reel: a story pass's suggestions show as related spans (proposed
// order, time, relevance; hide in-cut); "Use the proposed cut" adds them in one undo step; a
// span the human deletes is struck and never comes back; ✕ rejects, ↺ restores, ＋ adds one.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { hasLab, labMedia, openLabProject } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
const SEC = 705_600_000;
type Out = { ok?: boolean; output?: unknown; error?: unknown };

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!hasLab("preview"), "no lab media on this machine");

test("story panel", async ({ browser }) => {
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await openLabProject(page, "loc_4fadb756");
  await page.getByRole("button", { name: "Home" }).click();
  await page.locator('.project .item[data-framed="yes"]').filter({ hasText: "2% to 100%" }).first().getByRole("button", { name: "Open" }).click();
  await openTab(page, "Story");

  const chip = page.getByTestId("duration-chip");
  await expect(chip).toHaveClass(/inside/);
  const d0 = Number((await chip.textContent())!.replace(" s", ""));

  // Brief autosaves (after 0.8 s idle) and survives leaving and reopening the cut.
  await page.getByLabel("Brief").fill("GPU goes from 2% to 100%");
  await page.waitForTimeout(1500);
  await expect(page.getByTestId("saved")).toHaveText("Saved");

  // A typed out point: part 2 ends 0.5 s earlier.
  const out2 = page.getByLabel("Part 2 out", { exact: true });
  const v = await out2.inputValue();
  const [m, sec] = v.split(":");
  const t = Number(m) * 60 + Number(sec) - 0.5;
  await out2.fill(`${Math.floor(t / 60)}:${(t % 60).toFixed(2).padStart(5, "0")}`);
  await out2.press("Enter");
  await expect.poll(async () => Number((await chip.textContent())!.replace(" s", "")), { timeout: 20_000 }).toBeCloseTo(d0 - 0.5, 0);
  // › adds 0.5 s back.
  await page.getByRole("button", { name: "Part 2 out later" }).click();
  await expect.poll(async () => Number((await chip.textContent())!.replace(" s", "")), { timeout: 20_000 }).toBeCloseTo(d0, 0);
  // ↑ moves part 2 to the top.
  const rows = page.getByRole("list", { name: "Story parts" }).getByRole("listitem");
  const role2 = await rows.nth(1).locator(".role").textContent();
  await page.getByRole("button", { name: "Move part 2 up" }).click();
  await expect(rows.nth(0).locator(".role")).toHaveText(role2!);
  // ✕ deletes a part; ⌘Z brings it back.
  const n0 = await rows.count();
  await page.getByRole("button", { name: `Delete part ${n0}` }).click();
  await expect(rows).toHaveCount(n0 - 1);
  await page.locator("body").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(rows).toHaveCount(n0);

  await page.getByRole("button", { name: "Home" }).click();
  await page.locator('.project .item[data-framed="yes"]').filter({ hasText: "2% to 100%" }).first().getByRole("button", { name: "Open" }).click();
  await openTab(page, "Story");
  await expect(page.getByLabel("Brief")).toHaveValue("GPU goes from 2% to 100%");
  expect(errors).toEqual([]);
});

test("related spans and the proposed cut", async ({ browser }) => {
  test.skip(!existsSync(VIDEO), "no test video on this machine");
  test.setTimeout(180_000);
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
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<Out> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  const list = (await bridge("project_list_open", {})).output as { id: string; name: string; cuts: { id: string; title: string }[] }[];
  const p = list.find((x) => x.name === "h79-v4")!;
  const cut = p.cuts[0]!.id;
  const raw = (t: number) => ({ raw: { time: { clock: "src", t: t * SEC }, reason: "gather" } });
  const span = (a: number, role: string, rel: number) => ({ span: { start: raw(a), end: raw(a + 2) }, role, relevance: rel, quote: `span at ${a}` });
  // a gather's output, as the story pass writes it (the bridge stands in for the agent)
  const g = await bridge("story_suggestions_set", { cut, suggestions: [span(10, "setup", 0.4), span(20, "evidence", 0.9), span(30, "payoff", 0.6)], arc: [1, 0, 2] }, p.id);
  expect(g.ok, JSON.stringify(g)).toBeTruthy();

  await openTab(page, "Story");
  const spans = page.getByRole("list", { name: "Related spans" }).getByTestId("span-row");
  await expect(spans).toHaveCount(3);
  await expect(spans.nth(0)).toContainText("span at 20"); // proposed order
  await page.getByLabel("Sort spans").selectOption("time");
  await expect(spans.nth(0)).toContainText("span at 10");
  const rows = page.getByRole("list", { name: "Story parts" }).getByRole("listitem");
  const n0 = await rows.count();

  // use the proposed cut: three parts after the hook, one undo step
  await page.getByTestId("use-proposed").click();
  await expect(rows).toHaveCount(n0 + 3);
  await page.locator("body").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(rows).toHaveCount(n0);
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect(rows).toHaveCount(n0 + 3);
  await expect(spans.filter({ hasText: "✓" })).toHaveCount(3);

  // the human deletes the 20 s part: struck, and using the proposed cut again skips it
  const ins = await page.getByRole("list", { name: "Story parts" }).locator("input[aria-label$=' in']").evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value));
  const k = ins.findIndex((v) => v.startsWith("0:20"));
  expect(k).toBeGreaterThanOrEqual(0);
  await page.getByRole("button", { name: `Delete part ${k + 1}`, exact: true }).click();
  await expect(rows).toHaveCount(n0 + 2);
  await expect(page.locator(".spans .rejected")).toHaveCount(1);
  await page.getByTestId("use-proposed").click();
  await page.waitForTimeout(500);
  await expect(rows).toHaveCount(n0 + 2);

  // ↺ restores it as a suggestion; ＋ adds just that one
  await page.getByRole("button", { name: "Restore 0:20.00" }).click();
  await expect(page.locator(".spans .rejected")).toHaveCount(0);
  await page.getByRole("button", { name: "Add 0:20.00" }).click();
  await expect(rows).toHaveCount(n0 + 3);
  // hide in-cut: all three are in the cut now
  await page.getByLabel("Hide spans in the cut").check();
  await expect(spans).toHaveCount(0);
  expect(errors).toEqual([]);
});
