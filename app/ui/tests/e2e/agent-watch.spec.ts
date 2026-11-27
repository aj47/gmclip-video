// Decision 0059: agents at work, drawn. An outside agent's session edits the cut on
// screen (`dev_agent_tool`, as its connection would): each commit puts that run's cursor,
// named for the session, on the part it changed with what it did, and a ring on the
// part; your own edits draw nothing. An agent that passes through a cut that does not
// plan (it deletes every part, then adds new ones) raises no error: a plain "is
// mid-edit" line while it works, gone when the cut plans again. One that stops and leaves
// the cut broken gets the error, once it has been quiet for a while.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");
const S = 705_600_000; // flicks per second

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Out = { ok?: boolean; output?: Record<string, unknown> } & Record<string, unknown>;
type Tool = { ok?: Record<string, unknown>; error?: { code: string } };
type Parts = { as_of: number; parts: { id: string; src_in: number; src_out: number; media: unknown }[] };

test("an agent's edits are drawn, and its half-made cuts are not errors", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-watch-")), "agent-watch.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  // GMCLIP_SHOT_DIR: the run is filmed (the cursors are motion: a still does not show them)
  const film = process.env.GMCLIP_SHOT_DIR;
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, ...(film ? { recordVideo: { dir: film, size: { width: 1440, height: 900 } } } : {}) });
  const page = await context.newPage();
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    const g = globalThis as unknown as Record<string, unknown>;
    g.__GMCLIP_DEV_BRIDGE__ = info;
    // every error the window says, whenever it says it
    g.__alerts = [];
    new MutationObserver(() => {
      for (const t of document.querySelectorAll('.toast.error, [data-testid="cut-error"], .viewer-error')) {
        const line = `${t.className}: ${t.textContent}`;
        const a = g.__alerts as string[];
        if (!a.includes(line)) a.push(line);
      }
    }).observe(document, { subtree: true, childList: true, characterData: true });
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("agent-watch", { timeout: 30_000 });
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<Out> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  const agent = async (tool: string, args: Record<string, unknown>) => (await bridge("dev_agent_tool", { session: "thread-a", tool, args })).output as Tool;
  const alerts = () => page.evaluate(() => (globalThis as unknown as { __alerts: string[] }).__alerts);
  const list = (await bridge("project_list_open", {})).output as unknown as { id: string; name: string; cuts: { id: string }[] }[];
  const p = list.find((x) => x.name === "agent-watch")!;
  const cut = p.cuts[0]!.id;
  const parts = async () => (await agent("cut_get", { project: p.id, cut, projection: "parts" })).ok as unknown as Parts;
  let as_of = (await parts()).as_of;
  const pace = film ? 650 : 0;
  const run = async (command: string, args: Record<string, unknown>) => {
    const r = await agent("command_run", { project: p.id, command, args, base: { as_of } });
    expect(r.ok, `${command}: ${JSON.stringify(r)}`).toBeTruthy();
    as_of = (r.ok!.as_of as number | undefined) ?? as_of;
    if (pace) await page.waitForTimeout(pace);
    return r.ok!;
  };

  // your own edit draws nothing
  const layer = page.getByTestId("agent-layer");
  await bridge("cut_set", { cut, notes: "mine" }, p.id);
  await expect(layer).toHaveAttribute("data-cursors", "0");
  as_of = (await parts()).as_of;

  // the agent splits the part: its cursor, named for the session, on what it changed
  const first = (await parts()).parts[0]!.id;
  const made = (await run("part_split", { part: first, at: 30 * S })).output as { id: string };
  // (the ring first: it is only there for a moment)
  await expect(page.locator(`[data-testid="agent-ring"][data-entity="part:${made.id}"]`)).toHaveAttribute("data-op", "create");
  const cursor = page.getByTestId("agent-cursor");
  await expect(cursor).toHaveCount(1);
  await expect(cursor).toHaveAttribute("data-label", "Split");
  await expect(cursor).toHaveAttribute("data-name", "thread-a");
  await expect(cursor).toContainText("thread-a");
  expect([`part:${first}`, `part:${made.id}`]).toContain(await cursor.getAttribute("data-target"));
  // on the parts lane, inside the timeline
  const lanes = (await page.getByTestId("timeline-body").boundingBox())!;
  await expect.poll(async () => {
    const b = await cursor.boundingBox();
    return !!b && b.y >= lanes.y - 4 && b.y <= lanes.y + lanes.height && b.x >= lanes.x;
  }).toBe(true);
  // the layer never takes the pointer: the part under the cursor is still yours to click
  const at = (await cursor.boundingBox())!;
  await page.mouse.click(at.x + 3, at.y + 3);
  await expect(page.getByText(/^Part \d/i).first()).toBeVisible();

  // a trim says which edge; a delete leaves an outline where the part was
  for (const t of [50, 40, 20, 10]) await run("part_split", { part: t > 30 ? made.id : first, at: t * S });
  let now = await parts();
  as_of = now.as_of;
  await run("part_trim", { part: now.parts[1]!.id, src_in: 12 * S });
  await expect(cursor).toHaveAttribute("data-label", "Trim");
  await expect(cursor).toHaveAttribute("data-target", `part:${now.parts[1]!.id}`);
  await run("part_delete", { part: now.parts[3]!.id });
  await expect(cursor).toHaveAttribute("data-label", "Delete");
  await run("part_reorder", { part: now.parts[5]!.id, after: null });
  await run("cut_set", { cut, title: "Agent cut" });
  await expect(page.getByTestId("cut-title")).toHaveText("Agent cut");
  await expect(cursor).toHaveAttribute("data-changes", "9");
  await expect(page.getByTestId("agent-working")).toBeVisible();

  // a second session is a second cursor, in its own name, on what it changed
  now = await parts();
  const other = await bridge("dev_agent_tool", { session: "thread-b", tool: "command_run", args: { project: p.id, command: "part_trim", args: { part: now.parts[4]!.id, src_out: now.parts[4]!.src_out - 2 * S }, base: { as_of: now.as_of } } });
  expect((other.output as Tool).ok, JSON.stringify(other)).toBeTruthy();
  if (pace) await page.waitForTimeout(pace);
  await expect(cursor).toHaveCount(2);
  const b = page.locator('[data-testid="agent-cursor"][data-name="thread-b"]');
  await expect(b).toHaveAttribute("data-target", `part:${now.parts[4]!.id}`);
  await expect(b).toHaveAttribute("data-changes", "1");
  await expect(page.locator('[data-testid="agent-cursor"][data-name="thread-a"]')).toHaveAttribute("data-changes", "9");
  expect(await b.evaluate((el) => getComputedStyle(el).getPropertyValue("--agent-hue"))).not.toBe(await page.locator('[data-testid="agent-cursor"][data-name="thread-a"]').evaluate((el) => getComputedStyle(el).getPropertyValue("--agent-hue")));

  // it empties the cut and builds it again: said plainly, never an error
  now = await parts();
  as_of = now.as_of;
  const keep = now.parts.slice(0, 3).map((x) => ({ media: x.media, src_in: x.src_in, src_out: x.src_out }));
  for (const x of now.parts) await run("part_delete", { part: x.id });
  const mid = page.getByTestId("cut-mid-edit");
  await expect(mid).toContainText("thread-a is mid-edit");
  await expect(mid).toContainText("no parts");
  await expect(page.getByTestId("cut-error")).toHaveCount(0);
  for (const k of [...keep].reverse()) await run("part_add", { cut, ...k });
  await expect(mid).toHaveCount(0);
  await expect(page.getByTestId("cut-meta")).toHaveAttribute("data-parts", "3");
  // (past the time an error would have waited for the agents to settle)
  await page.waitForTimeout(5000);
  expect(await alerts()).toEqual([]);

  // it stops with the cut broken: the error is said once it has been quiet a while
  now = await parts();
  as_of = now.as_of;
  for (const x of now.parts) await run("part_delete", { part: x.id });
  await expect(mid).toBeVisible();
  await expect(page.getByTestId("cut-error")).toContainText("no parts", { timeout: 10_000 });
  await expect(mid).toHaveCount(0);
  expect(errors).toEqual([]);
  await context.close();
  // (the copy is 12 MB a run: not left in the temp folder)
  rmSync(dirname(copy), { recursive: true, force: true });
});
