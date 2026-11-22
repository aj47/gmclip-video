// Decision 0024, three openings, one short: on a 9:16 short made from a tapped moment, a
// second tapped moment becomes an alternate opening (a version sharing the rest of the
// cut); Compare shows them side by side; a version plays in the viewer and exports; the
// retention imported onto that export shows on its column only; Make main swaps it in
// (one undo step) and keeps the old opening as a version. Skips without the video.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab, rowOn } from "./tabs";

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

interface Versions {
  versions: { opening: string | null; label: string; main: boolean; from: string | null; sig: string; exports: number; retention: { at_3s: number } | null }[];
}

test("alternate openings side by side, exported, retention per opening, make main", async ({ browser }) => {
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
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 600_000 });
  await transcribed(page, "");
  const open = await bridge<{ id: string; name: string }[]>(page, "project_list_open", {});
  const pid = open.find((p) => p.name.includes("own-vm"))!.id;
  // two tapped moments, and a 9:16 short made on the first
  const stream = await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid);
  const m1 = await bridge<{ moment: { id: string } }>(page, "tap_mark", { cut: stream.cut, in_frame: 30, out_frame: 300 }, pid);
  await bridge(page, "tap_mark", { cut: stream.cut, in_frame: 420, out_frame: 720 }, pid);
  const made = await bridge<{ cut: string }>(page, "cut_from_moment", { moment: m1.moment.id }, pid);
  // the short: its hook (the first 3 s), then the rest of the take
  await bridge(page, "cut_split", { cut: made.cut, frame: 90 }, pid);
  const parts = (await bridge<{ parts: { id: string }[] }>(page, "cut_view", { cut: made.cut }, pid)).parts;
  await bridge(page, "part_set_role", { part: parts[1]!.id, role: "other" }, pid);
  await page.getByRole("button", { name: "Home" }).click();
  const projectCard = page.locator(".project").filter({ hasText: "own-vm" });
  await projectCard.locator(".item").filter({ hasNotText: /own-vm(__|-)1080/ }).first().getByRole("button", { name: "Open" }).click();
  await expect(page.getByTestId("cut-title")).toBeVisible();

  // Short tab › Openings: the main version, and an alternate from the second moment
  await openTab(page, "Short");
  await rowOn(page, "openings");
  const section = page.getByTestId("openings");
  await expect(section.getByTestId("opening-row")).toHaveCount(1, { timeout: 20_000 });
  await expect(section.getByTestId("openings-compare")).toBeDisabled();
  const from = section.getByTestId("opening-from");
  const momentValue = await from.locator("optgroup[label^='A moment'] option").nth(1).getAttribute("value");
  await from.selectOption(momentValue!);
  await section.getByTestId("opening-add").click();
  await expect(section.getByTestId("opening-row")).toHaveCount(2, { timeout: 20_000 });
  const v0 = await bridge<Versions>(page, "openings_view", { cut: made.cut }, pid);
  const alt = v0.versions.find((x) => !x.main)!;
  expect(alt.from).toBe("moment");

  // Compare: side by side
  await section.getByTestId("openings-compare").click();
  const sheet = page.getByTestId("openings-sheet");
  await expect(sheet).toBeVisible();
  const cols = sheet.getByTestId("opening-col");
  await expect(cols).toHaveCount(2);
  await expect(cols.first()).toHaveAttribute("data-main", "true");
  await expect(sheet).toContainText("Every version shares the rest of the cut (8.4 s)");
  for (const q of await sheet.getByTestId("opening-quote").allTextContents()) expect(q.length).toBeGreaterThan(4);
  // both columns sit side by side
  const [a, b] = [await cols.nth(0).boundingBox(), await cols.nth(1).boundingBox()];
  expect(Math.abs(a!.y - b!.y)).toBeLessThan(2);
  expect(b!.x).toBeGreaterThan(a!.x + a!.width - 1);
  await expect(cols.nth(1).getByTestId("opening-retention")).toContainText("No retention yet");

  // play the alternate in the viewer, then back to the cut
  await cols.nth(1).getByTestId("opening-play").click();
  await expect(sheet.getByTestId("opening-back")).toBeVisible();
  await sheet.getByTestId("opening-back").click();
  await expect(sheet.getByTestId("opening-back")).toHaveCount(0);

  // export the alternate; a retention curve imported onto that export is its own
  await cols.nth(1).getByTestId("opening-export").click();
  await expect
    .poll(async () => (await bridge<{ id: string; opening_sig?: string }[]>(page, "exports_list", {}, pid)).filter((e) => e.opening_sig === alt.sig).length, { timeout: 240_000 })
    .toBe(1);
  const exp = (await bridge<{ id: string; opening_sig?: string; opening_label?: string }[]>(page, "exports_list", {}, pid)).find((e) => e.opening_sig === alt.sig)!;
  expect(exp.opening_label).toBe(alt.label);
  const rows = ["Video position (%),Absolute audience retention (%)"];
  for (let p = 0; p <= 100; p += 5) rows.push(`${p},${(100 - p * 0.5).toFixed(1)}`);
  await bridge(page, "retention_import", { export: exp.id, name: "All.csv", text: rows.join("\n") }, pid);
  await sheet.getByRole("button", { name: "Close" }).click();
  await section.getByTestId("openings-compare").click();
  await expect(cols.nth(1).getByTestId("opening-retention")).toContainText("at 3 s", { timeout: 20_000 });
  await expect(cols.nth(0).getByTestId("opening-retention")).toContainText("No retention yet");

  // make it the main version: the old opening stays a version, one undo step
  await cols.nth(1).getByTestId("opening-choose").click();
  await expect(page.locator(".toast").filter({ hasText: "is the main version now" }).first()).toBeVisible({ timeout: 20_000 });
  await expect.poll(async () => (await bridge<Versions>(page, "openings_view", { cut: made.cut }, pid)).versions[0]!.sig, { timeout: 20_000 }).toBe(alt.sig);
  const v1 = await bridge<Versions>(page, "openings_view", { cut: made.cut }, pid);
  expect(v1.versions).toHaveLength(2);
  expect(v1.versions[1]!.from).toBe("was_main");
  expect(v1.versions[1]!.sig).toBe(v0.versions[0]!.sig);
  // the retention went with the opening: now on the main column
  expect(v1.versions[0]!.retention).not.toBeNull();
  expect(errors).toEqual([]);
});
