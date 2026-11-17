// QA workflow 3 (milestones/m2/qa-e2e.md): the agent edit pass from the launcher on a real
// stream, with each of the four client shapes played by a scripted stand-in (no model;
// GMCLIP_E2E_FAKE_CLIENTS=1, fixtures/fake-client.py): the run starts from the button,
// its "looking at" shows while it reads, it edits the cut through gmclip's tools, it ends
// with its summary; "Continue with a note" starts the next run (pi and opencode resume
// their session); and the History's "before an agent run" undoes it.
import { expect, test } from "@playwright/test";
import { bridge, currentCut, has, note, open, openVideo, projectNamed, QA, STREAM_A, timed } from "./qa-lib";
import { openTab } from "./tabs";

test.skip(!QA || !process.env.GMCLIP_E2E_FAKE_CLIENTS || !has(STREAM_A), "GMCLIP_QA=1, GMCLIP_E2E_FAKE_CLIENTS=1 and the loc_4fadb756 preview");

const NAME = "loc_4fadb756";

interface Run { started_ms: number; id: string; harness: string; state: string; summary: string | null; error: string | null; continued_from: string | null; resumed: boolean; tool_calls: number | null }

test("W3: agent edit pass with each client, continue, looking at, undo the run", async ({ browser }) => {
  test.setTimeout(900_000);
  const { page, errors } = await open(browser);
  await openVideo(page, STREAM_A, NAME);
  const pid = (await projectNamed(page, NAME))!.id;
  const cut = await currentCut(page, NAME);
  const title0 = (await page.getByTestId("cut-title").textContent())!;
  await openTab(page, "Agent");
  const rows = page.getByTestId("agent-run-row");
  // newest first, as the panel lists them
  const runs = async () => (await bridge<{ runs: Run[] }>(page, "agent_status", {}, pid)).runs.sort((a, b) => b.started_ms - a.started_ms);

  for (const [i, [h, file]] of ([["claude-code", "claude"], ["codex", "codex"], ["pi", "pi"], ["opencode", "opencode"]] as const).entries()) {
    await page.getByTestId("run-with").selectOption(h);
    await expect(page.getByTestId("client-path")).toContainText(`/fake-bin/${file}`);
    await timed(`${h}: run starts`, async () => {
      await page.getByTestId("agent-run-button").click();
      await expect(rows).toHaveCount(i + 1);
    });
    // what it is reading shows while it runs
    await expect(rows.first().getByTestId("looking-at")).toBeVisible({ timeout: 30_000 }).catch(async (e) => {
      note(`W3 ${h}: no looking-at while running (${JSON.stringify((await runs())[0])})`);
      throw e;
    });
    await expect(rows.first()).toHaveAttribute("data-state", "done", { timeout: 120_000 }).catch(async (e) => {
      note(`W3 ${h}: run did not finish: ${JSON.stringify((await runs())[0])}`);
      throw e;
    });
    await expect(rows.first()).toContainText(`${file}: retitled the cut (first run)`);
    await expect(page.getByTestId("cut-title")).toHaveText(`${file} pass (first)`, { timeout: 20_000 });
    note(`W3 ${h}: ${JSON.stringify((await runs())[0])}`);
  }

  // Continue with a note on the last two (opencode, then pi: both resume their session) and
  // on Claude Code (starts fresh with the summary).
  for (const file of ["opencode", "pi", "claude"]) {
    const row = rows.filter({ hasText: `${file}: retitled the cut (first run)` }).first();
    await row.getByTestId("continue-open").click();
    await page.getByLabel("Note to continue with").fill("continue: now make it 45 s");
    const n0 = await rows.count();
    await timed(`${file}: continue with a note`, async () => {
      await page.getByTestId("continue-run").click();
      await expect(rows).toHaveCount(n0 + 1);
    });
    await expect(rows.first()).toHaveAttribute("data-state", "done", { timeout: 120_000 });
    await expect(rows.first()).toContainText(`${file}: retitled the cut (continued run)`);
    const r = (await runs())[0]!;
    note(`W3 continue ${file}: ${JSON.stringify(r)}`);
    expect(r.continued_from).toBeTruthy();
    expect(r.resumed).toBe(file !== "claude");
  }
  await expect(page.getByTestId("cut-title")).toHaveText("claude pass (continued)");

  // Undo the run: History › "before an agent run" (the newest) restores the title before it.
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("History");
  await page.keyboard.press("Enter");
  const hist = page.getByRole("dialog", { name: "History" });
  const before = hist.locator(`[data-testid="history-row"][data-kind="run"]`);
  await expect(before.first()).toBeVisible();
  note(`W3 history rows: ${await hist.getByTestId("history-row").count()}, run rows ${await before.count()}`);
  await timed("restore before the last agent run", async () => {
    await before.first().getByRole("button", { name: /Restore/ }).click();
    await expect(page.getByTestId("cut-title")).toHaveText("pi pass (continued)", { timeout: 20_000 });
  });
  // and the oldest run's restore point brings back the stream's own title
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("History");
  await page.keyboard.press("Enter");
  await before.last().getByRole("button", { name: /Restore/ }).click();
  await expect(page.getByTestId("cut-title")).toHaveText(title0, { timeout: 20_000 });
  void cut;
  expect(errors).toEqual([]);
});
