// U.14 / A2.13: ⌘K finds the app's actions; ⌘⇧S names a version; the History dialog lists
// versions with what the cut was then and restores one (⌘Z undoes the restore). (A ⌘Z with
// nothing left in the session — after a reopen — opens the same dialog with a reason.)
// The slider across the versions shows each one's strip and length, and restores it.
// First, U.14 essentials: menu items reach the UI (keys list, undo, redo), ⌘S makes a
// restore point, the status reads Saved / Saving… / Not saved and a refused command does
// not turn it to Not saved.
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

/** What a native menu item sends (app_menu publishes the same `app` event). */
const menu = (page: Page, _s: unknown, id: string) =>
  page.evaluate(
    (id) =>
      (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<unknown> }).__gmclipBridgeCall("app_menu", { id }),
    id,
  );

test("menu, restore point, save status", async ({ browser }) => {
  test.setTimeout(120_000);
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
  const status = page.getByTestId("saved");
  await expect(status).toHaveText("Saved");
  const parts = page.getByTestId("cut-meta");

  // View › Keyboard Shortcuts.
  await menu(page, s, "keys");
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
  await page.keyboard.press("Escape");

  // Split, then Edit › Undo and Edit › Redo from the menu.
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }); // focus off the controls (its centre can be a button)
  for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:02\.00/);
  await page.keyboard.press("s");
  await expect(parts).toContainText("2 parts");
  await expect(status).toHaveText("Saved");
  await menu(page, s, "undo");
  await expect(parts).toContainText("1 part ");
  await menu(page, s, "redo");
  await expect(parts).toContainText("2 parts");
  // A refused command (nothing left to redo) is not a save failure.
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect(page.locator(".toast")).toBeVisible();
  await expect(status).toHaveText("Saved");

  // ⌘Z in a text field undoes the typing, not the project.
  await openTab(page, "Transcript"); // the side panel opens on Look
  const search = page.getByLabel("Search the transcript");
  await search.click();
  await page.keyboard.type("banana");
  await expect(search).toHaveValue("banana");
  await page.keyboard.press("ControlOrMeta+z");
  await expect(search).not.toHaveValue("banana");
  await expect(parts).toContainText("2 parts");
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }); // focus off the controls (its centre can be a button)

  // ⌘S: a restore point.
  await page.keyboard.press("ControlOrMeta+s");
  await expect(page.locator(".toast")).toContainText("restore point");
  await expect(status).toHaveText("Saved");
  expect(errors).toEqual([]);
});

// a copy of the reel: its own project, apart from the test above
test("palette, named versions, history restore", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-hist-")), "history-test.mp4");
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
  await expect(page.getByTestId("cut-title")).toHaveText("history-test", { timeout: 30_000 });
  const head = page.getByTestId("cut-meta");

  // ⌘K: find "keyboard" and run it
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByTestId("palette-input").fill("keyboard");
  await page.getByTestId("palette-input").press("Enter");
  const help = page.getByRole("dialog", { name: /Keyboard/ });
  await expect(help).toBeVisible();
  await help.getByRole("button", { name: "Close" }).click();
  await expect(help).toHaveCount(0);

  // ⌘K → "Developer: all commands": the core's catalogue, read only; Esc goes back to the
  // app's actions (which never list a core command)
  await page.keyboard.press("ControlOrMeta+k");
  const pin = page.getByTestId("palette-input");
  await pin.fill("strike words");
  await expect(page.getByTestId("palette-item").filter({ hasText: "words_strike" })).toHaveCount(0);
  await pin.fill("developer");
  await pin.press("Enter");
  await expect(page.getByTestId("palette-dev")).toBeVisible();
  await pin.fill("strike");
  const cmd = page.locator("[data-testid=palette-cmd][data-name=words_strike]");
  await expect(cmd).toBeVisible();
  await expect(cmd).toContainText("edit · undoable");
  await pin.press("Escape");
  await expect(page.getByTestId("palette-dev")).toHaveCount(0);
  await expect(page.getByTestId("palette-input")).toBeVisible();
  await pin.press("Escape");
  await expect(pin).toHaveCount(0);

  // ⌘⇧S: name this version (1 part)
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ControlOrMeta+Shift+s");
  await page.getByTestId("snapshot-name").fill("one take");
  await page.getByTestId("snapshot-name").press("Enter");
  const dlg = page.getByRole("dialog", { name: "History" });
  await expect(dlg.getByTestId("history-row").filter({ hasText: "one take" })).toContainText("1 parts", { timeout: 10_000 });
  await dlg.getByRole("button", { name: "Close" }).click();

  // two splits, then back to the named version from the History button
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  for (const n of [2, 3]) {
    for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
    // the split is at the frame the viewer shows: let the six steps land first (under
    // load the button read the frame before them and split where it had already split)
    await expect(page.getByTestId("timecode")).toHaveText(new RegExp(`^0:0${(n - 1) * 2}\\.00 `));
    await openTab(page, "Inspector");
    await page.getByRole("button", { name: "Split at playhead" }).click();
    await page.locator(".cut-head").hover();
    await expect.poll(async () => (await head.textContent()) ?? "").toContain(`${n} parts`);
  }
  // the cut head says how many versions there are and opens the same dialog
  await expect(page.getByTestId("earlier-versions")).toHaveText(/↶ \d+ earlier version/);
  await page.getByTestId("earlier-versions").click();
  const row = dlg.getByTestId("history-row").filter({ hasText: "one take" });
  await expect(row).toContainText("now 3 parts");
  await expect(row.getByTestId("history-by")).toContainText("you");
  // Compare: the cut then (1 part) and now (3): two parts added, one trimmed
  await row.getByRole("button", { name: /^Compare/ }).click();
  const d = row.getByTestId("history-diff");
  await expect(d).toContainText("then 1 parts");
  await expect(d).toContainText("now 3 parts");
  await expect(d).toContainText("2 added");
  await expect(d).toContainText("1 trimmed");
  await expect(d.locator(".hist-strip").nth(1).locator(".seg")).toHaveCount(3);
  await row.getByRole("button", { name: /Restore this cut/ }).click();
  await expect.poll(async () => (await head.textContent()) ?? "").toContain("1 part ");
  // the restore is one undo step
  await page.locator(".cut-head").hover();
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await head.textContent()) ?? "").toContain("3 parts");

  // Scrub through versions (lane K5): the slider starts at now (3 parts); Home is the
  // oldest version (1 part, one segment in its strip); → steps toward now; Restore
  // restores the one shown
  await page.getByTestId("earlier-versions").click();
  const scrub = dlg.getByTestId("history-scrub");
  await expect(scrub).toHaveAttribute("data-now", "1");
  await expect(scrub.getByTestId("history-scrub-label")).toHaveText("Now");
  await expect(scrub.getByTestId("history-scrub-length")).toContainText("3 parts");
  await expect(scrub.locator(".hist-strip .seg")).toHaveCount(3);
  await expect(scrub.getByTestId("history-scrub-restore")).toHaveCount(0);
  const slider = scrub.getByTestId("history-slider");
  await slider.focus();
  await page.keyboard.press("Home");
  await expect(scrub).toHaveAttribute("data-at", "0");
  await expect(scrub.getByTestId("history-scrub-length")).toContainText("1 part ·");
  await expect(scrub.getByTestId("history-scrub-length")).toContainText("against now");
  await expect(scrub.locator(".hist-strip .seg")).toHaveCount(1);
  await page.keyboard.press("ArrowRight");
  await expect(scrub).toHaveAttribute("data-at", "1");
  await page.keyboard.press("End");
  await expect(scrub).toHaveAttribute("data-now", "1");
  await page.keyboard.press("Home");
  await scrub.getByTestId("history-scrub-restore").click();
  await expect.poll(async () => (await head.textContent()) ?? "").toContain("1 part ");
  await expect(dlg).toHaveCount(0);
  expect(errors).toEqual([]);
});
