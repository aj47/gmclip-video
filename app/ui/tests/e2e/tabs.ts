// Opening a side-panel tab the way a person would (decision 0054): Words and Captions are
// the bar's tabs (when a bar is too narrow, its "More panels" menu has the ones past the
// edge); Look (off the bar since 2026-11-22), Story, Polish, Thumbnail, Review and Agent are ⌘K's "More panels". The names the
// panels had before are accepted: Transcript is Words, Tighten opens in the status dropdown,
// Inspector and Layout are Look, Short is Captions.
import { expect, type Page } from "@playwright/test";

/** A panel's tab now, by its name or an older one (layout.ts `tabFrom`). */
const TAB: Record<string, string> = {
  Transcript: "Words",
  Inspector: "Look",
  Layout: "Look",
  Short: "Captions",
};
const BAR = ["Words", "Captions"];

/** Opens a panel by its name (Words, Look, Captions, Story…; or Transcript, Inspector, Short…). */
export async function openTab(page: Page, name: string): Promise<void> {
  // Tighten is not a panel since 2026-11-22: it is in the cut's status dropdown
  if (name === "Tighten") {
    const pill = page.getByTestId("checks");
    if ((await pill.getAttribute("open")) === null) await pill.locator("summary").first().click();
    const fold = page.getByTestId("words-tighten");
    if ((await fold.getAttribute("open")) === null) await fold.locator("summary").click();
    await expect(fold).toHaveAttribute("open", "");
    return;
  }
  const tab = TAB[name] ?? name;
  const bar = page.locator(".side-tabs");
  const selected = bar.getByRole("tab", { name: tab, exact: true });
  if (BAR.includes(tab)) {
    const more = bar.getByRole("button", { name: "More panels" });
    // the bar measures itself after layout: wait until the tab or the menu is there
    await expect(selected.or(more).first()).toBeVisible({ timeout: 30_000 });
    if (await selected.isVisible()) await selected.click();
    else {
      await more.click();
      await page.getByRole("menuitem", { name: tab, exact: true }).click();
    }
  } else {
    await page.keyboard.press("ControlOrMeta+k");
    await page.getByTestId("palette-input").fill(`${tab} panel`);
    await page.getByTestId("palette-item").filter({ hasText: `${tab} panel` }).first().click();
  }
  await expect(selected).toHaveAttribute("aria-selected", "true");
}

/** A Captions row switched on (2026-11-22: each feature is a row whose settings show only
 * while it is on): highlight, hook, label, stopwatch, openings. */
export async function rowOn(page: Page, id: "highlight" | "hook" | "label" | "stopwatch" | "openings"): Promise<void> {
  const sw = page.getByTestId(`row-${id}`);
  await expect(sw).toBeVisible({ timeout: 20_000 });
  if (!(await sw.isChecked())) await sw.check();
  await expect(sw).toBeChecked();
}
