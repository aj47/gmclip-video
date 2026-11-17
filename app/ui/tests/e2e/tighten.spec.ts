// U.08 essentials: the audio lane, a pauses sweep (preview, untick one, cut the rest in
// one undo), h holds the screen, dragging a part moves it.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { analysed, transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

const head = async (page: Page) => (await page.getByTestId("cut-meta").textContent()) ?? "";
const nparts = async (page: Page) => Number(/(\d+) parts?/.exec(await head(page))?.[1] ?? NaN);
const secs = async (page: Page) => Number((await page.getByTestId("cut-meta").getAttribute("data-seconds")) ?? NaN);

test("timeline essentials", async ({ browser }) => {
  test.setTimeout(240_000);
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
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 180_000 });
  await transcribed(page, "");

  // The audio lane fills in (once the analysis has measured the level: by its progress,
  // which under load took longer than the 30 s this waited)
  await analysed(page, "");
  const tl = page.locator(".timeline-canvas");
  await expect.poll(async () => Number(await tl.getAttribute("data-audio-points")), { timeout: 30_000 }).toBeGreaterThan(300);

  // Pauses ≥ 0.4 s: find, untick the first, cut the rest; ⌘Z puts them back.
  const s0 = await secs(page);
  await openTab(page, "Tighten");
  await page.getByTestId("sweep-find").click();
  await expect(page.getByTestId("sweep-summary")).toContainText("found", { timeout: 30_000 });
  const rows = page.getByRole("list", { name: "Sweep candidates" }).getByRole("listitem");
  const n = await rows.count();
  expect(n).toBeGreaterThanOrEqual(2);
  await rows.first().getByRole("checkbox").uncheck();
  await expect(page.getByTestId("sweep-summary")).toContainText(`${n - 1} chosen`);
  await page.getByTestId("sweep-apply").click();
  await expect.poll(() => secs(page), { timeout: 20_000 }).toBeLessThan(s0 - 0.3);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } }); // focus off the controls (its centre can be a button)
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => secs(page), { timeout: 20_000 }).toBeCloseTo(s0, 1);

  // h at 4 s: the screen holds to the end of the sentence being said (hold_screen_here:
  // a split at the playhead, and one after the sentence unless it ends with the part);
  // h again removes it.
  await page.keyboard.press("Home");
  for (let i = 0; i < 12; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:04\.00/);
  await page.keyboard.press("h");
  await expect(page.locator(".toast")).toContainText("Screen held");
  await expect.poll(() => nparts(page)).toBeGreaterThanOrEqual(2);
  await page.keyboard.press("h");
  await expect(page.locator(".toast")).toContainText("Hold removed");

  // Drag part 2 before part 1.
  const mirror = page.getByRole("list", { name: "Parts" }).getByRole("button");
  const second = (await mirror.nth(1).textContent()) ?? "";
  const len2 = (() => { const m = /frames (\d+)–(\d+)/.exec(second)!; return Number(m[2]) - Number(m[1]); })();
  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const p2x = box.x + 12 + (Number(/frames (\d+)/.exec(second)![1]) + len2 / 2) * ppf;
  await page.mouse.move(p2x, box.y + 60);
  await page.mouse.down();
  await page.mouse.move(p2x - 60, box.y + 60, { steps: 4 });
  await page.mouse.move(box.x + 20, box.y + 60, { steps: 6 });
  await page.mouse.up();
  await expect(mirror.nth(0)).toHaveText(new RegExp(`frames 0–${len2}\\b`), { timeout: 20_000 });
  expect(errors).toEqual([]);
});
