// Shared helpers for the cross-feature QA workflows (`qa-*.spec.ts`, milestones/m2/qa-e2e.md).
// They drive AJ's real footage, read only, from the lab media folder; they are skipped unless
// GMCLIP_QA=1 (a 51-minute stream takes minutes to transcribe, which the normal e2e run
// should not pay for).
import { expect, type Browser, type Page } from "@playwright/test";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

/** loc_4fadb756 (Sep 14, 51 min, 720p H.264 + AAC): the h79 stream. */
export const STREAM_A = labMedia("preview/loc_4fadb756.mp4");
/** loc_086da141 (Sep 08, 53 min). */
export const STREAM_B = labMedia("preview/loc_086da141.mp4");

export const QA = !!process.env.GMCLIP_QA;
export const has = (...p: string[]) => p.every((x) => existsSync(x));

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

export interface Ctx {
  page: Page;
  errors: string[];
  state: { port: number; token: string; home: string; pid: number };
}

export async function open(browser: Browser, vp = { width: 1440, height: 900 }): Promise<Ctx> {
  const state = JSON.parse(readFileSync(STATE, "utf8")) as Ctx["state"];
  const page = await browser.newPage({ viewport: vp });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => {
    if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errors.push(m.text());
  });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: state.port, token: state.token });
  await page.goto("/");
  return { page, errors, state };
}

export async function bridge<T>(page: Page, command: string, args: Record<string, unknown> = {}, project = ""): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

/** Times an interactive step; anything over a second is logged as slow (qa-e2e.md). */
export async function timed<T>(what: string, f: () => Promise<T>): Promise<T> {
  const t = Date.now();
  const r = await f();
  const ms = Date.now() - t;
  const line = `${new Date().toISOString()} ${ms >= 1000 ? "SLOW" : "ok  "} ${ms} ms  ${what}\n`;
  if (process.env.GMCLIP_QA_LOG) appendFileSync(process.env.GMCLIP_QA_LOG, line);
  return r;
}

export function note(what: string) {
  if (process.env.GMCLIP_QA_LOG) appendFileSync(process.env.GMCLIP_QA_LOG, `${new Date().toISOString()} note ${what}\n`);
}

export interface Open {
  id: string;
  name: string;
  cuts: { id: string; title: string }[];
}

export async function projectNamed(page: Page, part: string): Promise<Open | undefined> {
  const list = await bridge<Open[]>(page, "project_list_open", {});
  return list.find((p) => p.name.includes(part) || part.includes(p.name));
}

/** The cut on screen, by the title in the header. */
export async function currentCut(page: Page, part: string): Promise<string> {
  const title = (await page.getByTestId("cut-title").textContent()) ?? "";
  const p = await projectNamed(page, part);
  return p?.cuts.find((c) => c.title === title)?.id ?? "";
}

/** Opens a video from Home's path field and waits for its transcript (minutes on a stream). */
export async function openVideo(page: Page, path: string, titlePart: string, transcribeMs = 900_000) {
  await page.getByLabel("Video path").fill(path);
  await timed(`open ${path.split("/").pop()} (cut view shows)`, async () => {
    await page.getByLabel("Video path").press("Enter");
    await expect(page.getByTestId("cut-title")).toContainText(titlePart, { timeout: 60_000 });
  });
  await timed(`transcribe ${path.split("/").pop()}`, async () => {
    await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: transcribeMs });
    await expect.poll(() => page.getByTestId("transcript").locator(".w").count(), { timeout: 60_000 }).toBeGreaterThan(100);
  });
}

/** Keys go to the editor, not a field: nothing focused (a click on the header can land on
 *  the title, which starts renaming the cut). */
export async function focusCut(page: Page) {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
}

export const frameOf = async (page: Page) => Number((await page.getByTestId("timecode").getAttribute("data-frame")) ?? NaN);
