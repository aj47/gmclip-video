// L.03 + L.04: a chat log on a stream's project fixture — imported, its strongest spike made
// a moment (origin chat) and a cut made from it, the way an agent does (chat_import,
// chat_moment, cut_from_moment) — then tap marking: `,` / `.` while the cut plays at 1× and
// at 2×, each pair a moment on sentence edges. Needs the lab media (read-only); skips
// without it.
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { hasLab, openCutFromHome, openLabProject } from "./lab";

const LOG = resolve(process.cwd(), "../src-tauri/tests/fixtures/chat/twitch-chat.json");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!hasLab("preview"), "no lab media on this machine");

type Moment = { id: string; origin: string; type: string; excerpt: string; start_s: number | null };

test("a chat spike becomes a moment and a cut; taps mark moments at 1× and 2×", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await openLabProject(page, "loc_4fadb756");
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<{ output?: unknown }> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  type Open = { id: string; name: string; cuts: { id: string; title: string }[] };
  const open = (await bridge("project_list_open", {})).output as Open[];
  const pid = open.find((p) => p.name.includes("loc_4fadb756"))?.id ?? open[0]!.id;
  const moments = async () => ((await bridge("moments_list", {}, pid)).output as { moments: Moment[] }).moments;
  const before = (await moments()).length;

  // the chat log: two spikes; the strongest (the laugh) becomes a moment, then a cut
  const logs = ((await bridge("chat_import", { path: LOG }, pid)).output as { logs: { owner: string; spikes: { start_s: number; end_s: number }[] }[] }).logs;
  const log = logs.at(-1)!;
  expect(log.spikes.length).toBe(2);
  const spike = log.spikes[0]!;
  const cm = (await bridge("chat_moment", { owner: log.owner, start_s: spike.start_s, end_s: spike.end_s, note: "the laugh" }, pid)).output as { id: string };
  const afterChat = await moments();
  expect(afterChat.length).toBe(before + 1);
  const chatMoment = afterChat.find((m) => m.id === cm.id)!;
  expect(chatMoment.origin).toBe("chat");
  expect(chatMoment.excerpt.length).toBeGreaterThan(0);
  const made = (await bridge("cut_from_moment", { moment: cm.id }, pid)).output as { cut: string };
  const title = ((await bridge("project_list_open", {})).output as Open[]).find((p) => p.id === pid)!.cuts.find((c) => c.id === made.cut)!.title;
  await page.getByRole("button", { name: "Home" }).click();
  await openCutFromHome(page, "loc_4fadb756", title);
  await expect(page.getByTestId("cut-title")).toHaveText(title);

  // tap marking while the cut plays, at 1× then 2×
  await expect(page.getByTestId("viewer-stats")).toContainText("×", { timeout: 30_000 });
  await bridge("viewer_seek", { frame: 0 });
  await bridge("viewer_play", {});
  await page.keyboard.press(",");
  await expect(page.getByText(/In marked/)).toBeVisible();
  await page.waitForTimeout(1500);
  await page.keyboard.press(".");
  await expect(page.getByText(/Moment marked/)).toBeVisible();
  await bridge("viewer_seek", { frame: 0 });
  await bridge("viewer_rate", { rate: 2 });
  await bridge("viewer_play", {});
  await page.keyboard.press(",");
  await page.waitForTimeout(1500);
  await page.keyboard.press(".");
  await expect(page.getByText(/Moment marked/)).toBeVisible();
  await bridge("viewer_rate", { rate: 1 });
  const marked = (await moments()).filter((m) => m.type === "marked");
  expect(marked.length).toBe(2);
  for (const m of marked) {
    expect(m.origin).toBe("manual");
    // whole sentences (a sentence ends at .?! or a pause ≥ 0.6 s)
    expect(m.excerpt.length).toBeGreaterThan(0);
  }
  expect(errors).toEqual([]);
});
