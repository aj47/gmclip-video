// Decision 0035: agent sessions over ACP against the scripted ACP agent
// (tests/e2e/fixtures/fake-acp.sh; no model, no network). The Agents workspace draws a
// session — status in plain words, its question, its plan, its last message — with the
// full log behind "Show log"; a session asks before a tool that is not gmclip's (full
// access) and waits, takes a follow-up and ends; gmclip access refuses that tool. In a
// project, the floating smiley (the developer setting's, decision 0054) opens a chat that
// starts a session on this cut with the default agent and shows the cut it edits. On Home, a big sun starts one on the library,
// with the model the agent offers picked before the start or switched while it runs.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { developerOn } from "./ux-lib";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(process.platform === "win32", "the scripted ACP agent is a /bin/sh script");

const shot = async (page: import("@playwright/test").Page, name: string) => {
  if (process.env.GMCLIP_SHOT_DIR) await page.screenshot({ path: `${process.env.GMCLIP_SHOT_DIR}/${name}.png` });
};

async function openPage(browser: import("@playwright/test").Browser) {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  return { page, errors };
}

test("a session: launch, see it work, answer, follow up, end", async ({ browser }) => {
  const { page, errors } = await openPage(browser);
  await page.getByTestId("nav-agents").click();
  await expect(page.getByTestId("agents-empty")).toContainText("Put your own agent to work");

  // the launch sheet: every agent (the scripted one stands in for all four), the library
  await page.getByTestId("session-new").click();
  const sheet = page.getByTestId("agent-launch");
  await expect(sheet.getByTestId("launch-agent")).toHaveCount(4);
  await expect(sheet.locator('[data-harness="claude-code"]')).toHaveAttribute("aria-checked", "true");
  await expect(sheet.getByTestId("launch-access-note")).toContainText("can only use gmclip");
  await sheet.getByTestId("launch-access").getByRole("button", { name: "Full access" }).click();
  await expect(sheet.getByTestId("launch-access-note")).toContainText("asks you first");
  await expect(sheet.getByTestId("launch-start")).toBeDisabled();
  await sheet.getByTestId("launch-message").fill("Find my best moment, and check the folder with a shell command");
  await sheet.getByTestId("launch-start").click();
  await expect(sheet).toHaveCount(0);

  // drawn, not logged: status, the question, the plan
  const pane = page.getByTestId("session-pane");
  await expect(page.getByTestId("session-asked")).toContainText("Find my best moment");
  const ask = page.getByTestId("entry-permission");
  await expect(ask).toContainText("wants to run `ls ~/Movies`");
  await expect(pane).toHaveAttribute("data-state", "waiting");
  await expect(page.getByTestId("session-status")).toHaveText("Needs your answer");
  await expect(page.getByTestId("entry-plan")).toContainText("List the projects");
  await expect(page.getByTestId("session-log")).toHaveCount(0);
  await expect(page.getByTestId("agents-badge")).toHaveText("1");
  await shot(page, "agents-ask");

  await ask.locator('[data-kind="allow_once"]').click();
  await expect(page.getByTestId("session-status")).toHaveText("Done — your turn");
  await expect(page.getByTestId("entry-agent")).toContainText("the strongest moment is at 12:04");
  await expect(page.getByTestId("entry-plan")).toContainText("2/2");
  await shot(page, "agents-session");

  // the power-user log: every message and tool call
  await page.getByTestId("session-log-toggle").click();
  const log = page.getByTestId("session-log");
  await expect(log.locator('[data-testid="entry-tool"].gm')).toContainText("gmclip · project_list");
  await expect(log.locator('[data-testid="entry-tool"]:not(.gm)')).toHaveAttribute("data-status", "completed");
  await expect(log.getByTestId("entry-permission")).toContainText("You chose: Allow");
  await shot(page, "agents-log");
  await page.getByTestId("session-log-toggle").click();
  await expect(log).toHaveCount(0);

  // a follow-up: Enter sends
  const input = page.getByTestId("session-input");
  await input.fill("Thanks, make a cut of it");
  await input.press("Enter");
  await expect(input).toHaveValue("");
  await expect(page.getByTestId("session-asked")).toContainText("Thanks, make a cut of it");
  await expect(page.getByTestId("session-status")).toHaveText("Done — your turn");

  await pane.getByLabel("Agent mode").selectOption("plan");
  await expect(pane.getByLabel("Agent mode")).toHaveValue("plan");

  await page.getByTestId("session-stop").click();
  await expect(page.getByTestId("session-state")).toHaveText("ended");
  await expect(page.getByTestId("agents-badge")).toHaveCount(0);

  // an ended session: a message picks it up again (`session/resume`), the transcript kept
  await expect(page.getByTestId("session-resume")).toBeVisible();
  await expect(input).toHaveAttribute("placeholder", /pick it up again/);
  await input.fill("Picking this back up");
  await input.press("Enter");
  await expect(input).toHaveValue("");
  await expect(page.getByTestId("session-asked")).toContainText("Picking this back up");
  await expect(page.getByTestId("session-status")).toHaveText("Done — your turn");
  await expect(pane.getByText("Find my best moment")).not.toHaveCount(0);
  await page.getByTestId("session-stop").click();
  await expect(page.getByTestId("session-state")).toHaveText("ended");

  // gmclip access: the same shell command is refused without asking
  await page.getByTestId("session-new").click();
  await sheet.getByTestId("launch-access").getByRole("button", { name: "gmclip tools only" }).click();
  await sheet.getByTestId("launch-message").fill("Same again with a shell");
  await sheet.getByTestId("launch-start").click();
  await expect(page.getByTestId("entry-notice")).toContainText("gmclip's tools only");
  await expect(page.getByTestId("entry-permission")).toHaveCount(0);
  await expect(page.getByTestId("session-status")).toHaveText("Done — your turn");
  await page.getByTestId("session-stop").click();
  await page.getByTestId("session-forget").click();
  await expect(page.getByTestId("session-row")).toHaveCount(1);

  // no asking: the shell command runs without a question, in the agent's no-ask mode
  await page.getByTestId("session-new").click();
  await sheet.getByTestId("launch-access").getByRole("button", { name: "No asking" }).click();
  await expect(sheet.getByTestId("launch-access-note")).toContainText("without asking you");
  await sheet.getByTestId("launch-message").fill("Bypass: check the folder with a shell command");
  await sheet.getByTestId("launch-start").click();
  await expect(page.getByTestId("session-status")).toHaveText("Done — your turn");
  await expect(page.getByTestId("entry-permission")).toHaveCount(0);
  await expect(page.getByTestId("session-pane")).toContainText("full access · no asking");
  await expect(pane.getByLabel("Agent mode")).toHaveValue("bypassPermissions");
  await page.getByTestId("session-log-toggle").click();
  await expect(page.getByTestId("session-log").locator('[data-testid="entry-tool"]:not(.gm)')).toHaveAttribute("data-status", "completed");
  await page.getByTestId("session-stop").click();
  // the sheet remembers it
  await page.getByTestId("session-new").click();
  await expect(sheet.getByTestId("launch-access-note")).toContainText("without asking you");
  expect(errors).toEqual([]);
});

const VIDEO = labMedia("h79/h79-v4.mp4");

test("the floating agent in a project: this cut, the default agent, the cut it edits", async ({ browser }) => {
  test.skip(!existsSync(VIDEO), "no test video on this machine");
  const { page, errors } = await openPage(browser);
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  // not in a cut by default: the top bar's Agents is the way in
  const fab = page.getByTestId("agent-fab");
  await expect(page.getByTestId("nav-agents")).toBeVisible();
  await expect(fab).toHaveCount(0);
  await developerOn(page);
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });

  await expect(fab).toBeVisible();
  await fab.click();
  const panel = page.getByTestId("agent-dock-panel");
  await expect(panel.getByTestId("dock-target")).toContainText("This cut: h79-v4");
  await expect(panel.getByTestId("dock-agent")).toHaveValue("claude-code");
  await expect(panel.getByTestId("dock-agent")).toContainText("Claude Code (default)");
  await shot(page, "dock-start");
  await panel.getByTestId("dock-message").fill("Tighten this cut");
  await panel.getByTestId("dock-message").press("Enter");

  // the session in the chat: the cut it works on, drawn
  const card = panel.getByTestId("work-card");
  await expect(card).toContainText("h79-v4", { timeout: 20_000 });
  await expect(panel.getByTestId("session-status")).toHaveText("Done — your turn");
  await expect(card).toContainText("2 changes");
  await expect(panel.getByTestId("dock-follow")).toBeChecked();
  await shot(page, "dock-session");
  await expect(fab).toHaveAttribute("aria-expanded", "true");

  // into the workspace
  await panel.getByTestId("dock-expand").click();
  await expect(page.getByTestId("session-pane")).toBeVisible();
  await expect(page.getByTestId("work-card")).toContainText("h79-v4");
  await shot(page, "agents-work");
  await page.getByTestId("session-stop").click();
  await expect(page.getByTestId("session-state")).toHaveText("ended");
  expect(errors).toEqual([]);
});

test("Home's big sun: a session on the library, with a model", async ({ browser }) => {
  const { page, errors } = await openPage(browser);
  const dock = page.getByTestId("agent-dock");
  await expect(dock).toHaveClass(/\bbig\b/);
  await expect(page.getByRole("complementary", { name: "Agent activity" })).toHaveCount(0);
  await shot(page, "home-sun");
  await page.getByTestId("agent-fab").click();
  const panel = page.getByTestId("agent-dock-panel");
  await expect(panel.getByTestId("dock-target")).toHaveValue("library");
  await expect(panel.getByTestId("dock-model")).toHaveValue("");
  await shot(page, "home-sun-open");
  await panel.getByTestId("dock-message").fill("What is in my library?");
  await panel.getByTestId("dock-message").press("Enter");
  await expect(panel.getByTestId("session-status")).toHaveText("Done — your turn", { timeout: 20_000 });
  // what the agent offers, switched while it runs
  const live = panel.getByTestId("dock-session-model");
  await expect(live).toHaveValue("fast");
  await live.selectOption("smart");
  await expect(live).toHaveValue("smart");
  await shot(page, "home-sun-session");
  await panel.getByTestId("dock-end").click();

  // the next start knows the models and picks one before it opens
  await panel.getByTestId("dock-new").click();
  await expect(panel.getByTestId("dock-model").locator("option")).toHaveText(["Default model", "Fast", "Smart"]);
  await panel.getByTestId("dock-model").selectOption("smart");
  await panel.getByTestId("dock-message").fill("Again, on the smart one");
  await panel.getByTestId("dock-message").press("Enter");
  await expect(panel.getByTestId("session-status")).toHaveText("Done — your turn", { timeout: 20_000 });
  await expect(panel.getByTestId("dock-session-model")).toHaveValue("smart");
  await panel.getByTestId("dock-end").click();
  expect(errors).toEqual([]);
});
