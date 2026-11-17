// A.02: "Grow story with my agent" runs the grow-story pass through the launcher (zero-spend
// stub model): the pass may only write story suggestions — its attempt to trim a part is
// refused — so the parts are unchanged after it; its spans and arc show under Related
// spans, and "Use the proposed cut" adds them.
import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
const SEC = 705_600_000;
const hasClaude = (() => {
  try {
    return /^2\.1\.300 /.test(execFileSync("claude", ["--version"], { encoding: "utf8" }));
  } catch {
    return false;
  }
})();

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");
test.skip(!hasClaude, "Claude Code 2.1.300 (the launcher's tested version) is not installed");

type Out = { ok?: boolean; output?: unknown; error?: unknown };

test("grow story proposes, the human applies", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<Out> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  const list = (await bridge("project_list_open", {})).output as { id: string; name: string; cuts: { id: string }[] }[];
  const p = list.find((x) => x.name === "h79-v4")!;
  const cut = p.cuts[0]!.id;
  const view = (await bridge("cut_view", { cut }, p.id)).output as { rev: number; parts: { id: string }[] };
  const part = view.parts[0]!.id;
  // what a real agent passes: the as_of of its last read
  const asOf = view.rev;
  const raw = (t: number) => ({ raw: { time: { clock: "src", t: t * SEC }, reason: "gather" } });
  const span = (a: number, role: string) => ({ span: { start: raw(a), end: raw(a + 3) }, role, relevance: 0.8, quote: `grown at ${a}` });
  // the pass: turn 1 tries to trim a part (refused), turn 2 writes its suggestions
  const t = await bridge("dev_stub_turns", { turns: [
    [["mcp__gmclip__command_run", { command: "part_trim", args: { part, src_out: 5 * SEC }, base: { as_of: asOf } }]],
    [["mcp__gmclip__command_run", { command: "story_suggestions_set", args: { cut, suggestions: [span(12, "setup"), span(40, "payoff")], arc: [1, 0] }, base: { as_of: asOf } }]],
  ] });
  expect(t.ok, JSON.stringify(t)).toBeTruthy();

  await openTab(page, "Story");
  const rows = page.getByRole("list", { name: "Story parts" }).getByRole("listitem");
  await expect(rows).toHaveCount(1);
  const n0 = 1;
  const out0 = await page.getByLabel("Part 1 out", { exact: true }).inputValue();
  await page.getByTestId("grow-story").click();
  const spans = page.getByRole("list", { name: "Related spans" }).getByTestId("span-row");
  await expect(spans).toHaveCount(2, { timeout: 180_000 });
  await expect(spans.nth(0)).toContainText("grown at 40"); // the proposed order
  // the trim was refused: the cut is as it was
  await expect(rows).toHaveCount(n0);
  await expect(page.getByLabel("Part 1 out", { exact: true })).toHaveValue(out0);
  await page.getByTestId("use-proposed").click();
  await expect(rows).toHaveCount(n0 + 2);
  expect(errors).toEqual([]);
});
