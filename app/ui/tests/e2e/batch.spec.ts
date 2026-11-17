// U.16 / E.01: "Send an agent…" on a project (the developer setting's, decision 0054) lists
// its cuts; a batch over three cuts runs them one at a time through the launcher (zero-spend
// stub model, never a real one) and shows each cut's state. Dev builds start it without the
// sheet (`dev_batch_start`).
import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { developerOn } from "./ux-lib";

const VIDEO = labMedia("h79/h79-v4.mp4");
const hasClaude = (() => {
  try {
    execFileSync("claude", ["--version"], { encoding: "utf8" });
    return true;
  } catch {
    return false;
  }
})();

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");
test.skip(!hasClaude, "Claude Code is not installed");

type Out = { ok?: boolean; output?: Record<string, unknown> } & Record<string, unknown>;

test("send an agent to three cuts", async ({ browser }) => {
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
  await developerOn(page);
  const row = page.locator(".project").filter({ hasText: "h79-v4" }).first();
  await row.getByTestId("batch-open").click();
  const sheet = page.getByRole("dialog", { name: "Send an agent to cuts" });
  await expect(sheet.getByTestId("batch-cut")).toHaveCount(1);
  await sheet.getByRole("button", { name: "Close" }).click();
  // two more cuts, as the human makes them
  const list = (await bridge("project_list_open", {})) as Out;
  const projects = list.output as unknown as { id: string; name: string; cuts: { id: string }[] }[];
  const p = projects.find((x) => x.name === "h79-v4")!;
  const cuts = [p.cuts[0]!.id];
  for (const t of ["second", "third"]) {
    const made = (await bridge("cut_create", { title: t }, p.id)) as Out;
    const out = made.output as { cut?: string; output?: { cut: string } };
    cuts.push(out.cut ?? out.output!.cut);
  }
  // the sheet's limits: out of range disables Start
  await row.getByTestId("batch-open").click();
  await sheet.getByTestId("batch-cut").first().check();
  await sheet.getByTestId("batch-per-cut").fill("120");
  await expect(sheet.getByTestId("batch-start")).toBeDisabled();
  await sheet.getByTestId("batch-per-cut").fill("45");
  await sheet.getByTestId("batch-budget").fill("2");
  await expect(sheet.getByTestId("batch-start")).toBeEnabled();
  await expect(sheet.getByTestId("batch-preapproval")).toContainText("$2.00");
  await sheet.getByRole("button", { name: "Close" }).click();
  const started = (await bridge("dev_batch_start", { cuts, pass: "edit_pass", budget_usd: 2, wall_clock_h: 4, per_cut_min: 45 }, p.id)) as Out;
  expect(started.ok ?? true).toBeTruthy();
  await row.getByTestId("batch-open").click();
  const rows = sheet.getByTestId("batch-row").first();
  await expect(rows.locator("p").first()).toContainText("· done", { timeout: 240_000 });
  // one at a time, each through the launcher with the stub model: all three done
  const items = rows.locator("li");
  await expect(items).toHaveCount(3);
  for (const t of ["h79-v4: done", "second: done", "third: done"]) await expect(rows).toContainText(t);
  await expect(rows.getByTestId("batch-limits")).toContainText("$2.00 · 4 h in all, 45 min a cut");
  expect(errors).toEqual([]);
});
