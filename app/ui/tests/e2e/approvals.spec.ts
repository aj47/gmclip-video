// A2.19 / R-PROTO-6 / R-UI-SEC-2: approval cards in the activity rail show what gmclip
// worked out the call would do, not what the agent says it does (its words are shown as
// unchecked text, markup inert); a run's 4th ask pauses it with "This agent keeps asking —
// review run", and "Let it continue" clears its asks; the same ask twice counts once
// (decision 0050). Dev builds inject the asks.
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { STATE } from "./global-setup";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

test("approval cards and the 4th-ask pause", async ({ browser }) => {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByTestId("nav-agents").click();
  await page.getByText("Outside agents and approvals").click();
  const ask = (prose: string, rev: number) =>
    page.evaluate(([p, rev]) =>
      (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<{ output: { queued: boolean; again?: boolean } }> }).__gmclipBridgeCall("dev_approval_add", {
        identity: "launched:e2e-run", tool: "export",
        effects: { cut: "01J00000000000000000000001", cut_rev: rev, blocked_by: [{ rule: "number_without_provenance", message: "the number 67 on screen has no provenance" }] },
        prose: p,
      }), [prose, rev] as const);
  const lie = 'This only renames a file. <img src=x onerror="window.__ap=1">';
  for (let i = 0; i < 3; i++) expect((await ask(lie, i)).output.queued).toBe(true);
  // the same ask again (decision 0050: same cut revision and findings) is not another
  expect((await ask(lie, 2)).output.again).toBe(true);
  expect((await ask(lie, 3)).output.queued).toBe(false);
  const rail = page.getByRole("complementary", { name: "Agent activity" });
  await rail.getByRole("button", { name: "Refresh activity" }).click();
  const cards = rail.getByTestId("approval");
  await expect(cards).toHaveCount(3);
  // the server's effects are what the card says it would do
  await expect(cards.first()).toContainText("blocked by number_without_provenance");
  // the agent's words: shown, unchecked, as text
  await expect(cards.first()).toContainText("This only renames a file.");
  await expect(cards.first().locator("img")).toHaveCount(0);
  expect(await page.evaluate(() => (globalThis as unknown as { __ap?: number }).__ap)).toBeUndefined();
  await expect(rail).toContainText("This agent keeps asking — review run");
  await cards.first().getByRole("button", { name: "Let it continue" }).click();
  await expect(cards).toHaveCount(0, { timeout: 10_000 });
  expect(errors).toEqual([]);
});
