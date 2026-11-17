// G.02: Settings lists every agent client gmclip can run (Claude Code, Codex, pi,
// opencode), each found by path — never a shell alias — with its version and whether a
// run may start on that build. The default agent is an Owner setting: from the dev
// bridge a change is refused (no Owner command runs there) and the default stays. Only `--version`
// is asked of any installed client; nothing runs. The style rules file's picker is an Owner
// command too (A.05).
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { STATE } from "./global-setup";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

test("your agents: listed, statuses, the default needs the Owner", async ({ browser }) => {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByRole("button", { name: "Settings" }).click();

  const picker = page.getByTestId("agent-picker");
  const rows = picker.getByTestId("agent-client");
  await expect(rows).toHaveCount(4, { timeout: 30_000 });
  for (const [h, label] of [["claude-code", "Claude Code"], ["codex", "Codex"], ["pi", "pi"], ["opencode", "opencode"]]) {
    const row = picker.locator(`[data-harness="${h}"]`);
    await expect(row).toContainText(label);
    const st = await row.getAttribute("data-status");
    // no client is refused outright any more, Codex included (decision 0012, 2026-10-16)
    expect(["ready", "allowed", "check", "blocked", "missing"]).toContain(st);
    // a client that is not there says how to point gmclip at it
    if (st === "missing") await expect(row).toContainText("set its path");
  }
  await expect(picker.getByTestId("agent-default")).toHaveCount(1);
  await expect(picker.locator('[data-harness="claude-code"]').getByTestId("agent-default")).toHaveCount(1);

  // Making another installed client the default is an Owner change: no sheet here.
  const other = picker.locator('[data-testid="agent-client"]:not([data-harness="claude-code"])').filter({ has: page.getByRole("button", { name: "Make default…" }) });
  if (await other.count()) {
    await other.first().getByRole("button", { name: "Make default…" }).click();
    await expect(page.locator(".toast")).toContainText("no Owner command runs from the dev bridge");
    await expect(picker.locator('[data-harness="claude-code"]').getByTestId("agent-default")).toHaveCount(1);
  }

  // Setting a client's path is an Owner change too: refused at the dev bridge.
  await picker.locator('[data-harness="pi"]').getByRole("button", { name: "Set path…" }).click();
  await picker.getByLabel("pi path").fill("/opt/homebrew/bin/pi");
  await picker.getByRole("button", { name: "Use it…" }).click();
  await expect(page.locator(".toast")).toContainText("no Owner command runs from the dev bridge");

  // A.05: the style rules file is picked in the native panel and pinned on the native sheet
  // (Owner): the dev bridge refuses it and nothing is pinned.
  await expect(page.getByTestId("settings-style")).toHaveText("none");
  await page.getByTestId("style-pick").click();
  await expect(page.locator(".toast").last()).toContainText("no Owner command runs from the dev bridge");
  await expect(page.getByTestId("settings-style")).toHaveText("none");
  expect(errors).toEqual([]);
});
