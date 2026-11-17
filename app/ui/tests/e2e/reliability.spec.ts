// Lane S, SPEC §14 / §5.12 / R-ERR-2: on a full disk (the project's store at a simulated
// full disk through the dev bridge) edits keep working, the chip says "Not saved: disk
// full" in red and retries, and when the space comes back it turns Saved with every edit
// still there. With 1.5 GB free (an injected statfs) a new export is refused with the
// numbers. Home's "Restore an earlier copy" lists a project's copies and puts one
// back (the state before kept as a copy). Settings › Storage shows what gmclip keeps and
// offers "Free up" for rebuildable caches only.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

test("disk full keeps edits, restore a copy, storage", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-rel-")), "reliable.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("reliable", { timeout: 30_000 });
  const open = await bridge<{ id: string; name: string }[]>(page, "project_list_open", {}, "");
  const pid = open.find((p) => p.name.includes("reliable"))?.id ?? "";
  expect(pid).toBeTruthy();
  const chip = page.getByTestId("saved");
  await expect(chip).toHaveText("Saved", { timeout: 20_000 });
  const head = page.getByTestId("cut-meta");

  // the disk fills: splits keep landing, the chip goes red and says why
  await bridge(page, "dev_disk_full", { on: true }, pid);
  // the viewer has the cut (its length in the timecode) before keys seek it
  await expect(page.getByTestId("timecode")).not.toContainText("/ 0:00.00", { timeout: 30_000 });
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  let parts = 1;
  for (let i = 0; i < 40; i++) {
    await page.locator(".cut-head").hover();
    for (let k = 0; k < 4; k++) await page.keyboard.press("Shift+ArrowRight");
    await openTab(page, "Inspector");
    await page.getByRole("button", { name: "Split at playhead" }).click();
    parts++;
    await expect.poll(async () => (await head.textContent()) ?? "").toContain(`${parts} parts`);
    if (((await chip.textContent()) ?? "").includes("disk full")) break;
  }
  await expect(chip).toHaveText("Not saved: disk full", { timeout: 10_000 });
  await expect(chip).toHaveClass(/failed/);
  // still retrying, nothing dropped
  await page.waitForTimeout(1500);
  await expect(chip).toHaveText("Not saved: disk full");
  await expect(head).toContainText(`${parts} parts`);

  // space returns: Saved again, every split still there
  await bridge(page, "dev_disk_full", { on: false }, pid);
  await expect(chip).toHaveText("Saved", { timeout: 10_000 });
  await expect(head).toContainText(`${parts} parts`);

  // R-ERR-2 low disk, fault-injected statfs: with 1.5 GB "free" a new export is refused
  // with both numbers and nothing starts; with room again it is not refused for space
  await bridge(page, "dev_free_space", { bytes: 1_500_000_000 }, pid);
  await page.getByTestId("export").click();
  const low = page.locator(".toast.error").filter({ hasText: "Not enough free space" });
  await expect(low).toContainText("1.5 GB free, this export needs 2.0 GB", { timeout: 20_000 });
  await bridge(page, "dev_free_space", { bytes: null }, pid);

  // a copy now (the timer's job, made on demand), then Home › Other ways to open › Restore an earlier copy
  const made = await bridge<{ backup: { id: string } | null }>(page, "backup_now", {}, pid);
  expect(made.backup).not.toBeNull();
  await page.getByRole("button", { name: "Home" }).click();
  await page.getByTestId("open-more").click();
  await page.getByTestId("restore-open").click();
  const sheet = page.getByTestId("restore-sheet");
  await expect(sheet.getByTestId("restore-project").filter({ hasText: "reliable" })).toBeVisible();
  const proj = sheet.getByTestId("restore-project").filter({ hasText: "reliable" });
  await expect(proj.getByTestId("restore-copy")).toHaveCount(1);
  await proj.getByTestId("restore-pick").first().click();
  await expect(sheet.getByTestId("restore-confirm")).toBeVisible();
  await sheet.getByTestId("restore-go").click();
  await expect(page.locator(".toast").filter({ hasText: "Restored" })).toBeVisible({ timeout: 20_000 });
  // the state it replaced is kept as a copy: two copies now, one "before a restore"
  await expect(proj.getByTestId("restore-copy")).toHaveCount(2, { timeout: 10_000 });
  await expect(proj.locator("[data-reason=before-restore]")).toHaveCount(1);
  await sheet.getByRole("button", { name: "Close" }).click();

  // Settings › Storage
  await page.getByRole("button", { name: "Settings" }).click();
  const st = page.getByTestId("storage");
  await expect(st.locator("[data-id=projects]")).toContainText("kept");
  await expect(st.locator("[data-id=proxies]")).toContainText("can be freed");
  await expect(st.getByTestId("storage-free")).toBeVisible();
  await expect(page.getByTestId("settings-setup")).toBeVisible();
  expect(errors).toEqual([]);
});
