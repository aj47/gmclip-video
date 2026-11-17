// Decision 0050: an outside agent's session runs the app's own commands as itself. Its
// export that lint blocks (a hook with a number nobody measured) is one ask however often
// it retries, waits on Home's "Needs you" and the top bar's badge, and "Approve and
// export" exports it as you; the job is then the agent's to wait on. Dev builds make the
// agent's calls (`dev_agent_tool`: the MCP tools as a session's connection would).
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Out = { ok?: boolean; output?: Record<string, unknown> } & Record<string, unknown>;
type Tool = { ok?: Record<string, unknown>; error?: { code: string; plan?: Record<string, unknown> } };

test("an agent's blocked export: one ask, on Home, approved and exported", async ({ browser }) => {
  test.setTimeout(300_000);
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
  const agent = async (tool: string, args: Record<string, unknown>) => (await bridge("dev_agent_tool", { session: "e2e-a", tool, args })).output as Tool;
  const list = (await bridge("project_list_open", {})).output as unknown as { id: string; name: string; cuts: { id: string }[] }[];
  const p = list.find((x) => x.name === "h79-v4")!;
  const cut = p.cuts[0]!.id;

  // the agent's hook, through the app's own command, then its export: refused, once
  const hook = await agent("command_run", { command: "hook_set", project: p.id, args: { cut, text: "We grew 67 percent" } });
  expect(hook.ok, JSON.stringify(hook)).toBeTruthy();
  const asks: unknown[] = [];
  for (let i = 0; i < 2; i++) {
    const r = await agent("command_run", { command: "export_start", project: p.id, args: { cut } });
    expect(r.error?.code, JSON.stringify(r)).toBe("NeedsApproval");
    asks.push(r.error?.plan?.approval);
  }
  expect(asks[1]).toBe(asks[0]);
  // a dry run says what blocks it and asks nobody
  const dry = await agent("export", { project: p.id, cut, dry_run: true });
  expect(dry.ok?.blocked, JSON.stringify(dry)).toBe(true);

  // Home's Needs you and the badge
  await page.getByRole("button", { name: "Home" }).click();
  const item = page.getByTestId("needs-approval");
  await expect(item).toHaveCount(1);
  await expect(item).toContainText("local:session-e2e-a");
  await expect(page.getByTestId("agents-badge")).toHaveText("1");
  await item.getByTestId("needs-approval-export").click();
  await expect(item).toHaveCount(0);
  await expect(page.getByTestId("agents-badge")).toHaveCount(0);

  // exported as you, though lint blocks the agent's export
  let job = "";
  await expect
    .poll(async () => {
      const jobs = (await bridge("jobs_list", {})).output as unknown as { id: string; cut: string; state: string }[];
      const j = jobs.find((x) => x.cut === cut);
      job = j?.id ?? "";
      return j?.state;
    }, { timeout: 240_000, intervals: [1000] })
    .toBe("done");
  // and the job is the agent's to wait on
  const waited = await agent("command_run", { command: "job_wait", args: { job } });
  expect(waited.ok?.state, JSON.stringify(waited)).toBe("done");
  expect(errors).toEqual([]);
});
