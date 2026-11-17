// U.16: "Run with my agent" on the cut (edit pass), run progress and result, Run again,
// a --media grant is refused without the native sheet (none in the dev bridge), and the
// MCP connect lines. Zero-spend stub model; nothing runs before a click.
import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
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

test("agent buttons and the connect lines", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");

  // Connect lines on Home.
  await page.getByTestId("open-more").click();
  await page.getByTestId("connect-agent").click();
  await expect(page.getByTestId("connect-claude")).toContainText(/^claude mcp add --scope user gmclip -- .*gmclip-app mcp$/);
  await expect(page.getByTestId("connect-codex")).toContainText(/^codex mcp add gmclip -- .*gmclip-app mcp$/);
  await page.getByRole("button", { name: "Close" }).click();

  // Settings: what agents run under.
  await page.getByRole("button", { name: "Settings" }).click();
  await expect(page.getByTestId("settings-style")).toHaveText("none");

  // Settings › Agent access: an Owner change from the dev bridge is refused (no sheet, never).
  await page.getByTestId("agent-access").click();
  await page.getByLabel("Folder for agents").fill("/tmp");
  await page.getByRole("button", { name: "Add folder…" }).click();
  await expect(page.locator(".toast")).toContainText("no Owner command runs from the dev bridge");
  await page.getByRole("button", { name: "Close" }).click();
  await page.getByRole("button", { name: "Home" }).click();

  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");

  await openTab(page, "Agent");
  await expect(page.getByText("No runs yet.")).toBeVisible();
  await expect(page.getByTestId("client-path")).toContainText(/Claude Code: \/.*\/claude/);
  // Edit pass: runs, finishes, shows its result.
  await page.getByTestId("agent-run-button").click();
  const rows = page.getByTestId("agent-run-row");
  await expect(rows.first()).toHaveAttribute("data-state", "done", { timeout: 180_000 });
  await expect(rows.first()).toContainText("Edit pass");
  await expect(rows.first()).toContainText("From your agent");
  // Run again.
  await rows.first().getByRole("button", { name: "Run again" }).click();
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toHaveAttribute("data-state", "done", { timeout: 180_000 });

  // A media grant needs the native sheet: refused here (no window).
  await page.getByLabel("Let the agent see and hear the footage (asks you first)").check();
  await page.getByTestId("agent-run-button").click();
  await expect(page.locator(".toast")).toContainText("not approved");
  await expect(rows).toHaveCount(2);
  expect(errors).toEqual([]);
});
