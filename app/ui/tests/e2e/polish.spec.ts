// Edit polish (Ideas 2026-10-16, K2.1–K2.5; decision 0025) on a real recording: the
// Polish tab lists stressed words with what was measured (★ Star, ⌘Z), the punch-in
// switch adds and removes zooms, a callout goes in with one click, the loop option shows
// the last frame against the opening, and "Find screen jumps" reads the frames. Uses the
// lab media folder (GMCLIP_LAB_MEDIA, default ~/Development/gmclip-lab-media).
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { openTab } from "./tabs";

const LAB = process.env.GMCLIP_LAB_MEDIA || join(homedir(), "Development/gmclip-lab-media");
const VIDEO = join(LAB, "renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no lab media on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

test("the Polish tab: stressed words, punch-in, callouts, loop, screen jumps", async ({ browser }) => {
  test.setTimeout(600_000);
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
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  let pid = "";
  await expect
    .poll(async () => {
      pid = (await bridge<{ id: string; name: string }[]>(page, "project_list_open", {})).find((p) => p.name.includes("own-vm"))?.id ?? "";
      if (!pid) return false;
      const st = await bridge<{ sources: { transcript: boolean }[] }>(page, "analysis_status", {}, pid);
      return st.sources.length > 0 && st.sources.every((x) => x.transcript);
    }, { timeout: 300_000, intervals: [2_000] })
    .toBe(true);
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 60_000 });
  await transcribed(page, "");

  await openTab(page, "Polish");
  const panel = page.getByRole("region", { name: "Polish" });
  await expect(panel).toBeVisible();

  // stressed words: each says what it measured; ★ Star, then ⌘Z
  const rows = page.getByTestId("polish-stress");
  await expect.poll(() => rows.count(), { timeout: 60_000 }).toBeGreaterThan(0);
  const words = await page.getByTestId("polish-emphasis").locator(".polish-row").count();
  expect(words).toBeLessThanOrEqual(30);
  await expect(rows.first().locator(".polish-why")).toContainText(/louder|slower/);
  const star = rows.first().getByTestId("polish-star");
  await expect(star).toHaveText("Star");
  await star.click();
  await expect(star).toHaveText("Starred", { timeout: 20_000 });
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(star).toHaveText("Star", { timeout: 20_000 });

  // punch-in: the per-cut switch adds every suggestion, then takes them away
  const punch = page.getByTestId("polish-punch-on");
  if (await punch.isEnabled()) {
    await punch.check();
    await expect(punch).toBeChecked({ timeout: 20_000 });
    await expect(page.getByTestId("polish-punch-remove").first()).toBeVisible();
    await punch.uncheck();
    await expect(punch).not.toBeChecked({ timeout: 20_000 });
  }

  // callouts: one click adds one
  const callouts = page.getByTestId("polish-callout");
  if ((await callouts.count()) > 0) {
    const add = callouts.first().getByTestId("polish-callout-add");
    await add.click();
    await expect(add).toHaveText("Added", { timeout: 20_000 });
  }

  // loop: on, and the last frame is compared with the opening
  const loop = page.getByTestId("polish-loop-on");
  await loop.check();
  await expect(loop).toBeChecked({ timeout: 20_000 });
  await expect(page.getByTestId("polish-loop-state")).toContainText(/opening/);

  // screen jumps: the frames are read and the answer says how many
  await page.getByTestId("polish-holds-find").click();
  await expect(page.getByTestId("polish-holds-summary")).toBeVisible({ timeout: 240_000 });
  await expect(page.getByTestId("polish-holds-summary")).toContainText("frames read");
  const holds = page.getByTestId("polish-hold");
  console.log(`polish: ${await rows.count()} stressed, ${await page.getByTestId("polish-punch-row").count()} punch-ins, ${await callouts.count()} callouts, ${await holds.count()} holds — ${await page.getByTestId("polish-holds-summary").textContent()}`);
  if (process.env.POLISH_SHOT) await panel.screenshot({ path: process.env.POLISH_SHOT });
  if ((await holds.count()) > 0) {
    const h = holds.first().getByTestId("polish-hold-apply");
    await h.click();
    await expect(h).toHaveText("Held", { timeout: 20_000 });
  }
  expect(errors).toEqual([]);
});
