// Decision 0050: History's "Undo this agent" undoes one outside agent's run (`run_undo`),
// and when you changed some of the same things since, it offers the core's choices: here
// "Undo only what nobody changed since" takes the agent's title back and keeps your note.
// Dev builds make the agent's calls (`dev_agent_tool`, as its session's connection would).
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Out = { ok?: boolean; output?: Record<string, unknown> } & Record<string, unknown>;
type Tool = { ok?: Record<string, unknown>; error?: { code: string } };

test("undo this agent, with the choices when you edited since", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-undo-")), "agent-undo.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("agent-undo", { timeout: 30_000 });
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<Out> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  const agent = async (tool: string, args: Record<string, unknown>) => (await bridge("dev_agent_tool", { session: "e2e-u", tool, args })).output as Tool;
  const list = (await bridge("project_list_open", {})).output as unknown as { id: string; name: string; cuts: { id: string }[] }[];
  const p = list.find((x) => x.name === "agent-undo")!;
  const cut = p.cuts[0]!.id;

  // the agent retitles the cut and notes it, in one run
  const read = await agent("cut_get", { project: p.id, cut });
  let as_of = read.ok?.as_of as number;
  for (const args of [{ cut, title: "Agent title" }, { cut, notes: "the agent's note" }]) {
    const r = await agent("command_run", { project: p.id, command: "cut_set", args, base: { as_of } });
    expect(r.ok, JSON.stringify(r)).toBeTruthy();
    as_of = r.ok!.as_of as number;
  }
  await expect(page.getByTestId("cut-title")).toHaveText("Agent title", { timeout: 10_000 });
  // then you write your own note over the agent's
  await bridge("cut_set", { cut, notes: "my note" }, p.id);

  await page.locator(".cut-head").hover();
  await page.getByTestId("earlier-versions").click();
  const dlg = page.getByRole("dialog", { name: "History" });
  const row = dlg.getByTestId("history-row").filter({ has: page.getByTestId("history-undo-agent") }).first();
  await expect(row).toContainText("before agent:session-e2e-u");
  await row.getByTestId("history-undo-agent").click();
  const choice = row.getByTestId("history-undo-choice");
  await expect(choice).toBeVisible();
  await expect(choice.getByTestId("history-undo-all")).toBeVisible();
  await choice.getByTestId("history-undo-untouched").click();
  await expect(page.getByTestId("cut-title")).toHaveText("agent-undo", { timeout: 10_000 });
  const after = await agent("cut_get", { project: p.id, cut });
  expect(JSON.stringify(after.ok)).toContain("my note");
  expect(errors).toEqual([]);
});
