// Lane S, SPEC §12.5: a stranger's first run. Home shows a welcome card until setup is
// done; the setup sheet asks four questions with pictures (what you record → kit, where
// you post → shapes/presets/loudness, find moments with your agent or yourself, where your
// recordings are), shows the speech model's state, and lists one line per default before
// the one system dialog (which the dev bridge never shows: Apply is refused there). After
// the first real import Home shows the five-step checklist, dismissible. The sheet fits a
// small and a narrow window.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

async function noSideScroll(page: Page, what: string) {
  const over = await page.evaluate(() => {
    const out: string[] = [];
    if (document.documentElement.scrollWidth > innerWidth + 1) out.push(`page ${document.documentElement.scrollWidth} > ${innerWidth}`);
    const d = document.querySelector<HTMLElement>("[data-testid=setup-sheet]");
    if (d && d.scrollWidth > d.clientWidth + 1) out.push(`sheet ${d.scrollWidth} > ${d.clientWidth}`);
    return out;
  });
  expect(over, what).toEqual([]);
}

test("the four questions, the summary, the checklist", async ({ browser }) => {
  test.setTimeout(180_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1100, height: 700 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");

  // the welcome card, until setup is done
  await expect(page.getByTestId("welcome")).toBeVisible();
  await page.getByTestId("setup-open").click();
  const sheet = page.getByTestId("setup-sheet");
  await expect(sheet).toBeVisible();
  // the speech model block (ready here: the e2e links the real model cache)
  await expect(sheet.getByTestId("setup-model")).toBeVisible();
  // four questions, each with picture tiles
  for (const q of ["q-kit", "q-platforms", "q-finder", "q-recordings"]) await expect(sheet.getByTestId(q)).toBeVisible();
  await expect(sheet.getByTestId("q-kit").locator("svg.fr-pic")).toHaveCount(4);
  await expect(sheet.getByTestId("q-platforms").locator("svg.fr-pic")).toHaveCount(4);
  // detected agents come from the launcher; "I'll pick moments myself" is always there
  await expect(sheet.getByTestId("agent-self")).toBeVisible();
  await expect(sheet.getByTestId("q-finder").getByRole("radio")).not.toHaveCount(0);
  // nothing to apply until every question is answered
  await expect(sheet.getByTestId("setup-go")).toBeDisabled();
  await expect(sheet.getByTestId("setup-summary")).toContainText("Answer the four questions");

  await sheet.getByTestId("kit-gaming-variety").click();
  await expect(sheet.getByTestId("kit-gaming-variety")).toHaveAttribute("aria-checked", "true");
  await sheet.getByTestId("platform-tiktok").click();
  await sheet.getByTestId("platform-youtube").click();
  await sheet.getByTestId("agent-self").click();
  await sheet.getByLabel("Recordings folder").fill(tmpdir());
  // one line per default
  const sum = sheet.getByTestId("setup-summary");
  await expect(sum.locator("dd[data-id=kit]")).toContainText("Gameplay + facecam");
  await expect(sum.locator("dd[data-id=layout]")).toContainText("camera across the top");
  await expect(sum.locator("dd[data-id=platforms]")).toContainText("TikTok, YouTube (16:9)");
  await expect(sum.locator("dd[data-id=framings]")).toContainText("9:16 and 16:9");
  await expect(sum.locator("dd[data-id=loudness]")).toContainText("−14 LUFS");
  await expect(sum.locator("dd[data-id=finder]")).toContainText("you pick them");
  await expect(sum.locator("dd[data-id=media]")).toContainText("asked per run");
  await expect(sum.locator("dd[data-id=recordings]")).toContainText("never write");
  await noSideScroll(page, "1100×700");
  for (const vp of [{ width: 800, height: 600 }, { width: 600, height: 900 }]) {
    await page.setViewportSize(vp);
    await noSideScroll(page, `${vp.width}×${vp.height}`);
    await expect(sheet.getByTestId("setup-go")).toBeVisible();
  }
  await page.setViewportSize({ width: 1100, height: 700 });
  // Apply asks the Owner in a system dialog; the dev bridge has none, so it is refused
  await expect(sheet.getByTestId("setup-go")).toBeEnabled();
  await sheet.getByTestId("setup-go").click();
  await expect(page.locator(".toast, [role=alert]").filter({ hasText: /not allowed|Owner/ }).first()).toBeVisible();
  await sheet.getByRole("button", { name: "Later" }).click();
  await expect(sheet).toHaveCount(0);

  // the checklist after the first real import
  if (existsSync(VIDEO)) {
    await expect(page.getByTestId("checklist")).toHaveCount(0);
    const copy = join(mkdtempSync(join(tmpdir(), "gmclip-first-")), "first-run.mp4");
    copyFileSync(VIDEO, copy);
    await page.getByLabel("Video path").fill(copy);
    await page.getByLabel("Video path").press("Enter");
    await expect(page.getByTestId("cut-title")).toHaveText("first-run", { timeout: 30_000 });
    await page.getByRole("button", { name: "Home" }).click();
    const cl = page.getByTestId("checklist");
    await expect(cl).toBeVisible({ timeout: 10_000 });
    await expect(cl.getByTestId("checklist-step")).toHaveText(["Make it vertical", "Pick a moment", "Trim by words", "Check captions", "Export"]);
    // a portrait video opens vertical: the layout step is done
    await expect(cl.locator("[data-step=layout]")).toHaveAttribute("data-done", "yes");
    await expect(cl.locator("[data-step=export]")).toHaveAttribute("data-done", "no");
    await cl.getByTestId("checklist-dismiss").click();
    await expect(cl).toHaveCount(0);
  }
  expect(errors).toEqual([]);
});
