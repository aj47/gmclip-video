// Decision 0024, title and description drafts: "Draft with my agent" in the Thumbnail tab
// runs the publish_copy pass through the launcher with a scripted stand-in client (no
// model; GMCLIP_E2E_FAKE_CLIENTS=1, fixtures/fake-client.py). It speaks the pass contract
// (ai_pass begin_self, then pass_step with one draft per platform); every draft lands as a
// proposal; the human accepts one, edits one (an edit accepts it) and rejects one.
// Nothing is published.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!process.env.GMCLIP_E2E_FAKE_CLIENTS, "GMCLIP_E2E_FAKE_CLIENTS=1 (scripted agent clients)");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("title and description drafts from the agent, decided by the human", async ({ browser }) => {
  test.setTimeout(1_200_000);
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
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 600_000 });
  await transcribed(page, "");

  await openTab(page, "Thumbnail");
  const panel = page.getByTestId("copy-drafts");
  await expect(panel.getByTestId("copy-empty")).toBeVisible();
  await panel.getByTestId("copy-agent").click();
  // the run proposes one draft per platform; the panel shows them as proposals
  const drafts = panel.getByTestId("copy-draft");
  await expect.poll(async () => {
    await panel.getByRole("button", { name: "Refresh drafts" }).click();
    return drafts.count();
  }, { timeout: 180_000, intervals: [2_000] }).toBe(4).catch(async (e: unknown) => {
    // say what the run did before failing
    const st = await page.evaluate(() => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: object) => Promise<unknown> }).__gmclipBridgeCall("agent_status", {}));
    console.log(JSON.stringify(st).slice(0, 4000));
    throw e;
  });
  await expect(panel.getByTestId("copy-platform")).toHaveCount(4);
  for (const st of await drafts.evaluateAll((els) => els.map((e) => e.getAttribute("data-state")))) expect(st).toBe("proposed");
  // grounded in the cut's own words: the scripted client writes from the first line it
  // was served, which is what the cut's transcript opens with
  const first = ((await page.getByTestId("transcript").locator(".w").first().textContent()) ?? "").trim();
  expect(first.length).toBeGreaterThan(0);
  await expect(drafts.first().locator(".copy-desc")).toContainText(first);

  // accept YouTube's, edit TikTok's (accepts it), reject Instagram's
  const on = (p: string) => panel.locator(`[data-testid="copy-platform"][data-platform="${p}"]`).getByTestId("copy-draft");
  await on("youtube").getByTestId("copy-accept").click();
  await expect(on("youtube")).toHaveAttribute("data-state", "accepted");
  await on("tiktok").getByRole("button", { name: "Edit" }).click();
  await on("tiktok").getByLabel("Title").fill("My PC made this in under 2 minutes");
  await on("tiktok").getByTestId("copy-save").click();
  await expect(on("tiktok")).toHaveAttribute("data-state", "accepted");
  await expect(on("tiktok")).toContainText("My PC made this in under 2 minutes");
  await expect(on("tiktok")).toContainText("agent, edited");
  await on("instagram").getByRole("button", { name: "Reject" }).click();
  await expect(on("instagram")).toHaveAttribute("data-state", "rejected");
  await expect(on("shorts")).toHaveAttribute("data-state", "proposed");
  expect(errors).toEqual([]);
});
