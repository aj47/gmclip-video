// Decision 0024, hook text with measured numbers: the Short tab offers hook lines built on
// numbers the clip shows — a measured run first, then numbers said in it — each naming
// where its number comes from; a spoken duration the measured run contradicts is flagged.
// One click puts a line on the cut as its text hook. Skips without the video.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

test("hook lines on measured and spoken numbers, one click each", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toHaveText("h79-v4", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  const open = await bridge<{ id: string; name: string; cuts: { id: string; title: string }[] }[]>(page, "project_list_open", {});
  const proj = open.find((p) => p.name.includes("h79-v4"))!;
  const cut = proj.cuts.find((c) => c.title === "h79-v4")!.id;

  await openTab(page, "Short");
  const lines = page.getByTestId("hook-line");
  // spoken numbers first: "…in less than two minutes" is said in the clip
  await expect(lines.first()).toBeVisible({ timeout: 30_000 });
  await expect(lines.filter({ hasText: "2 minutes" }).first()).toBeVisible();
  await expect(lines.filter({ hasText: "2 minutes" }).first()).toContainText("said");

  // a run measured from the stream (12 s → 79.4 s): its line comes first, and the
  // spoken "two minutes" is flagged against it
  await bridge(page, "run_measure_src", { cut, from_s: 12, to_s: 79.4 }, proj.id);
  await openTab(page, "Inspector");
  await openTab(page, "Short");
  await expect(lines.first()).toHaveAttribute("data-kind", "measured", { timeout: 20_000 });
  await expect(lines.first()).toContainText("Watch it finish in 67 seconds");
  await expect(lines.first()).toContainText("a measured run: 67.4 s");
  await expect(lines.filter({ hasText: "2 minutes" }).first().getByTestId("hook-line-warning")).toContainText("67.4 s");
  for (const t of await lines.locator(".hook-line-text").allTextContents()) {
    const n = t.trim().split(/\s+/).length;
    expect(n, t).toBeGreaterThanOrEqual(5);
    expect(n, t).toBeLessThanOrEqual(8);
  }

  // one click: the line is the cut's hook, with its number's provenance
  await lines.first().click();
  const hookRow = page.locator('.overlay-row[data-role="text_hook"]');
  await expect(hookRow).toContainText("Watch it finish in 67 seconds", { timeout: 20_000 });
  await expect(page.getByLabel("Hook text")).toHaveValue("Watch it finish in *67 seconds*");
  const sug = await bridge<{ suggestions: { text: string; provenance: { measurement: string | null }[] }[] }>(page, "hook_text_suggest", { cut }, proj.id);
  expect(sug.suggestions[0]!.provenance[0]!.measurement).toBeTruthy();
  expect(errors).toEqual([]);
});
