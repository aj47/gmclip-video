// U.10 essentials, folded into the cut (decision 0054): after an export the export popover
// shows the file (Reveal, Share…, drag out) and its verify, and re-verifies the file (H.06);
// an agent run's handoff shows in the status pill as text (R-UI-SEC-2) with a Restore per
// choice; another agent edit makes the export stale and its check offers it again. The
// Agents view's activity rail offers Pause agents, and the review link panel (a developer
// setting) asks for the native sheet: both refused over the dev bridge.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("export → the file and the agent's note in the cut; Pause needs the Owner", async ({ browser }) => {
  test.setTimeout(400_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");

  // Activity rail in the Agents view (Home has the big sun instead).
  await page.getByTestId("nav-agents").click();
  await page.getByText("Outside agents and approvals").click();
  const rail = page.getByRole("complementary", { name: "Agent activity" });
  await expect(rail).toBeVisible();
  await expect(rail.getByTestId("agents-pause")).toBeVisible();
  await rail.getByTestId("agents-pause").click();
  await expect(page.locator(".toast.error")).toContainText("dev bridge");
  await expect(rail.getByTestId("agents-paused")).toHaveCount(0);

  await page.getByRole("button", { name: "Home" }).click();
  await expect(page.getByRole("complementary", { name: "Agent activity" })).toHaveCount(0);

  // Settings › Developer (decision 0054): off, no review link panel and no Pair with Clippy;
  // on, both. The review link panel: not serving; serving needs the native sheet (none
  // here); pairing with Clippy is the Owner's (refused over the bridge).
  await page.getByRole("button", { name: "Settings" }).click();
  const rl = page.getByRole("region", { name: "Review link" });
  const dev = page.getByTestId("pref-developer");
  await expect(dev).not.toBeChecked();
  await expect(rl).toHaveCount(0);
  await expect(page.getByTestId("clippy-pair")).toHaveCount(0);
  await dev.click();
  await page.getByTestId("clippy-pair").click();
  await expect(page.locator(".toast.error")).toContainText("dev bridge");
  await expect(rl.getByTestId("review-state")).toContainText("Not serving");
  await rl.getByTestId("review-serve").click();
  await expect(page.locator(".toast.error")).toBeVisible({ timeout: 10_000 });
  await expect(rl.getByTestId("review-state")).toContainText("Not serving");
  await dev.click();
  await expect(rl).toHaveCount(0);
  await page.getByRole("button", { name: "Home" }).click();

  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await page.getByTestId("export").click();
  await expect(page.getByTestId("export-done")).toBeVisible({ timeout: 360_000 });
  // H.06 (R-EXP-6): the export's verify, and a re-verify of the file against the plan
  await page.getByRole("button", { name: "Export options" }).click();
  const ver = page.getByTestId("export-verify");
  await expect(ver.getByTestId("export-verify-line")).toContainText("Verified at export: verified:");
  await ver.getByTestId("export-reverify").click();
  await expect(ver.getByTestId("export-verify-line")).toContainText(/Re-verified at .*: \d+ checks passed/, { timeout: 60_000 });
  await expect(ver).toHaveAttribute("data-passed", "true");
  await ver.locator("summary").click();
  await expect(ver.locator(".verify-checks li[data-ok=true]").filter({ hasText: "sha256: matches the export record" })).toHaveCount(1);
  await expect(ver.locator(".verify-checks li[data-ok=false]")).toHaveCount(0);
  // the newest export's file, in the popover (the board's Reveal / Share, decision 0054)
  const file = page.getByTestId("export-file");
  await expect(file).toContainText("h79-v4");
  await expect(file.getByTestId("export-reveal")).toBeEnabled();
  // Share… needs the app window (the e2e runs headless): it says so
  await file.getByTestId("export-share").click();
  await expect(page.locator(".toast.error")).toContainText("no window", { timeout: 10_000 });
  await page.getByRole("button", { name: "Export options" }).click();
  await expect(page.getByTestId("export-done").getByTestId("export-done-reveal")).toBeVisible();

  // A.04 / R-CC-4: an agent run on the cut shows on the cut, its handoff summary in the
  // status pill in the quarantined container (R-UI-SEC-2), and a recorded choice restores
  // in one click.
  type Reply = { ok: boolean; output?: unknown; error?: unknown };
  const call = async <T,>(command: string, args: Record<string, unknown>, project?: string): Promise<T> => {
    const r = await page.evaluate(
      ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: unknown, p?: string) => Promise<Reply> }).__gmclipBridgeCall(c as string, a, p as string | undefined),
      [command, args, project] as const,
    );
    if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
    return r.output as T;
  };
  const open = await call<{ id: string; cuts: { id: string; title: string }[] }[]>("project_list_open", {});
  const pid = open.find((p) => p.cuts.some((c) => c.title === "h79-v4"))!.id;
  const cut = open.find((p) => p.id === pid)!.cuts.find((c) => c.title === "h79-v4")!.id;
  const view = await call<{ parts: { id: string }[] }>("cut_view", { cut }, pid);
  const part = view.parts[0]!.id;
  await call("dev_agent_call", { command: "part_set_gain", args: { part, gain_db: 6 } }, pid);
  const said = '<img src=x onerror="window.__xss=3"> Started on the result';
  await call("dev_agent_call", {
    command: "run_summary_set",
    args: { cut, text: said, reversible_choices: [{ label: "Lifted part one <b>6 dB</b>", restore: [{ entity: `part:${part}`, field: "gain_db", value: 0 }] }] },
  }, pid);
  expect((await call<{ agent_working: { identity: string } | null }>("cut_view", { cut }, pid)).agent_working?.identity).toBe("local:e2e");
  await expect(page.getByTestId("agent-working")).toBeVisible();
  const pill = page.getByTestId("checks");
  await expect(pill.getByTestId("handoff-chip")).toBeVisible({ timeout: 20_000 });
  await pill.locator("summary").click();
  const handoff = pill.getByTestId("handoff");
  await expect(handoff).toContainText(said);
  await expect(handoff).toContainText("Lifted part one <b>6 dB</b>");
  await expect(handoff.locator("img, b")).toHaveCount(0);
  await handoff.getByTestId("choice-restore").click();
  await expect(page.locator(".toast").last()).toContainText("Restored");
  await handoff.getByTestId("choice-restore").click();
  await expect(page.locator(".toast").last()).toContainText("Already as recorded");
  expect(await page.evaluate(() => (globalThis as unknown as { __xss?: number }).__xss)).toBeUndefined();

  // A2.17: another agent edit makes the export stale; its check offers the export again.
  await call("dev_agent_call", { command: "part_set_gain", args: { part, gain_db: 3 } }, pid);
  await expect(pill.locator('[data-rule="export_stale"]').getByTestId("check-fix")).toHaveText("Export again", { timeout: 20_000 });
  expect(errors).toEqual([]);
});
