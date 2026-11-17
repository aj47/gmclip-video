// L.06: a short on an opened reel, consolidated the way an agent does it (project_consolidate:
// a folder with only the stretches the short uses; Home has no Consolidate… since decision
// 0054), and Home's "Open a consolidated project…" opens that folder as a project of its own.
// Skips without the reel.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");
const SEC = 705_600_000;

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

test("consolidate a project and open the copy", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  const open = await bridge<{ id: string; name: string; cuts: { id: string }[] }[]>(page, "project_list_open", {});
  const p = open.find((x) => x.name.includes("h79"))!;
  const stream = p.cuts[0]!.id;
  void stream;
  const st = await bridge<{ sources: { source: string }[] }>(page, "analysis_status", {}, p.id);
  const media = { source: st.sources[0]!.source };
  const cut = (await bridge<{ cut: string }>(page, "cut_create", { title: "two parts" }, p.id)).cut;
  const first = (await bridge<{ id: string }>(page, "part_add", { cut, media, src_in: 3 * SEC, src_out: 6 * SEC }, p.id)).id;
  await bridge(page, "part_add", { cut, media, after: first, src_in: 40 * SEC, src_out: 43 * SEC }, p.id);
  await bridge(page, "caption_set", { cut, enabled: false }, p.id);
  await bridge(page, "framing_create", { cut, name: "16:9", w: 1280, h: 720, fps: { num: 30, den: 1 } }, p.id);

  const made = await bridge<{ home: string; sources: { how: string }[] }>(page, "project_consolidate", { handles_s: 5 }, p.id);
  expect(made.sources.filter((x) => x.how === "trimmed")).toHaveLength(1);
  const home = made.home;
  await page.getByRole("button", { name: "Home" }).click();

  await page.getByTestId("open-more").click();
  await page.getByTestId("open-consolidated").click();
  await page.getByLabel("Consolidated project folder").fill(home);
  await page.getByTestId("consolidate-open").click();
  await expect(page.getByText("Opened the consolidated project")).toBeVisible();
  await expect(page.locator(".project").filter({ hasText: "(consolidated)" })).toBeVisible();
  // the copy's cut plans from the trimmed file
  const all = await bridge<{ id: string; name: string; cuts: { id: string; title: string }[] }[]>(page, "project_list_open", {});
  const copy = all.find((x) => x.name.includes("(consolidated)"))!;
  expect(copy.cuts.some((c) => c.id === cut)).toBe(true);
  const cv = await bridge<{ frames: number }>(page, "cut_view", { cut }, copy.id);
  expect(cv.frames).toBe(180);
  expect(errors).toEqual([]);
});
