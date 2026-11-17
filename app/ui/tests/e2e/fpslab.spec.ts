// AJ's file that failed on 2026-10-15 (lab: his machine only, read only — nothing is
// written beside it; exports go to the test's throwaway HOME): a 67 min 1440p restream,
// fragmented MP4 on a 10 kHz clock with 330–340 tick frames. It opens without a cut
// error, a 10 s span of it plans, and the span exports.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia, labSkipLine } from "./lab";

const REL = "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-05-2026-restream.mp4";
const VIDEO = labMedia(REL);
/** The file a project is opened from: the app names it after the link's target. */
const REAL = existsSync(VIDEO) ? realpathSync(VIDEO) : VIDEO;

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), labSkipLine(REL));

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

interface Open {
  id: string;
  name: string;
  cuts: { id: string; title: string }[];
}

test("AJ's restream opens, plans and exports a span", async ({ browser }) => {
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
  const sheet = page.getByRole("dialog", { name: "Import" });
  if (await sheet.isVisible({ timeout: 10_000 }).catch(() => false)) await sheet.getByTestId("import-open").click();
  await expect(page.getByTestId("cut-title")).toContainText("mmo-where-bots", { timeout: 60_000 });
  await expect(page.getByTestId("cut-error")).toHaveCount(0);
  await expect.poll(async () => Number((await page.getByTestId("cut-meta").getAttribute("data-seconds")) ?? 0), { timeout: 30_000 }).toBeGreaterThan(3600);

  // a 10 s span: the one part trimmed to 60–70 s
  const title = (await page.getByTestId("cut-title").textContent()) ?? "";
  const list = await bridge<Open[]>(page, "project_list_open", {}, "");
  const p = list.find((x) => REAL.includes(x.name))!;
  const cut = p.cuts.find((c) => c.title === title)!.id;
  const view = await bridge<{ fps: { num: number; den: number }; parts: { id: string }[] }>(page, "cut_view", { cut }, p.id);
  expect(view.fps).toEqual({ num: 30, den: 1 });
  const part = view.parts[0]!.id;
  await bridge(page, "part_time_set", { cut, part, edge: "out", text: "70" }, p.id);
  await bridge(page, "part_time_set", { cut, part, edge: "in", text: "60" }, p.id);
  await expect.poll(async () => Number((await page.getByTestId("cut-meta").getAttribute("data-seconds")) ?? 0), { timeout: 30_000 }).toBeCloseTo(10, 1);
  await expect(page.getByTestId("cut-error")).toHaveCount(0);

  await page.getByTestId("export").click();
  await expect(page.locator(".export-state")).toContainText("Exported", { timeout: 240_000 });
  await expect(page.getByTestId("export-failed")).toHaveCount(0);
  expect(errors).toEqual([]);
});
