// L.05 "New cut like this": a 9:16 short made on an opened video, then More → New cut
// like this… on another moment, saved as a look; the new cut opens in the same framing
// with one part, and the saved look is offered the next time. Skips without the video.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

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

test("new cut like this, saved as a look", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");
  const open = await bridge<{ id: string; name: string }[]>(page, "project_list_open", {});
  const pid = open.find((p) => p.name.includes("own-vm"))!.id;
  // two moments (tapped), and a 9:16 short made on the first
  const stream = await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid);
  const m1 = await bridge<{ id: string }>(page, "tap_mark", { cut: stream.cut, in_frame: 30, out_frame: 300 }, pid);
  await bridge(page, "tap_mark", { cut: stream.cut, in_frame: 420, out_frame: 720 }, pid);
  const made = await bridge<{ cut: string }>(page, "cut_from_moment", { moment: (m1 as unknown as { moment: { id: string } }).moment.id }, pid);
  // open it from Home
  await page.getByRole("button", { name: "Home" }).click();
  const projectCard = page.locator(".project").filter({ hasText: "own-vm" });
  const shortRow = projectCard.locator(".item").filter({ hasNotText: /own-vm(__|-)1080/ }).first();
  await shortRow.getByRole("button", { name: "Open" }).click();
  await expect(page.getByTestId("cut-title")).toBeVisible();

  // New cut like this… (the head's More menu)
  await page.getByTestId("view-menu").click(); // "View and more" (2026-11-22: View folded into ⋯)
  await page.getByTestId("cut-like-open").click();
  const sheet = page.getByTestId("cutlike-sheet");
  await expect(sheet).toBeVisible();
  const rows = sheet.getByTestId("cutlike-moment");
  await expect(rows).toHaveCount(2);
  await rows.nth(1).click();
  await sheet.getByTestId("cutlike-save").check();
  await sheet.getByLabel("Template name").fill("Tap look");
  await sheet.getByTestId("cutlike-make").click();
  await expect(page.getByText(/cut with this look · saved as “Tap look”/)).toBeVisible();
  await expect(sheet).toHaveCount(0);

  // the new cut: one part, the short's 9:16 framing
  const cuts = (await bridge<{ id: string; name: string; cuts: { id: string; title: string; parts: number; framed: boolean }[] }[]>(page, "project_list_open", {}))
    .find((p) => p.id === pid)!.cuts;
  const fresh = cuts.filter((c) => c.id !== made.cut && c.id !== stream.cut);
  expect(fresh.length).toBe(1);
  expect(fresh[0]!.parts).toBe(1);
  const look = async (cut: string) => (await bridge<{ framing: { aspect: string; list: unknown[] } }>(page, "cut_view", { cut }, pid)).framing;
  const [a, b] = [await look(fresh[0]!.id), await look(made.cut)];
  expect(a.aspect).toBe("9:16");
  expect(a.aspect).toBe(b.aspect);
  expect(a.list.length).toBe(b.list.length);

  // the saved look is offered next time (⌘K, in a narrow window: the More menu)
  await page.setViewportSize({ width: 820, height: 860 });
  await page.getByTestId("view-menu").click(); // "View and more" (2026-11-22: View folded into ⋯)
  await page.getByTestId("cut-like-open").click();
  await expect(page.getByTestId("cutlike-sheet").getByLabel("Look from")).toContainText("Tap look");
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});
