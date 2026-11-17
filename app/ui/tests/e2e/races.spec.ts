// The app races the flaky specs found (lane K9, 2026-10-17), each made to happen every
// time instead of under load: a gesture's preview letting go before its commit is read
// back (the overlay or crop box jumped back for a moment, and a drag begun then hit the
// old place), and ↑ / ↓ reading the viewer's last reported frame instead of the seek just
// asked for (↓ ↓ landed on the same part).
import { expect, test, type Browser, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const REEL = labMedia("h79/h79-v4.mp4");
const WIDE = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

async function open(browser: Browser, video: string, title: string | RegExp): Promise<{ page: Page; errors: string[] }> {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(video);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText(title, { timeout: 30_000 });
  return { page, errors };
}

/** Starts recording, on every change under `root`, the first element matching `sel`
 * (and containing `has`): its text, or its left edge. */
async function record(page: Page, root: string, sel: string, what: "text" | "x", has = "") {
  return page.evaluateHandle(
    ([root, sel, what, has]) => {
      const seen: (string | number)[] = [];
      const read = () => {
        const el = [...document.querySelectorAll(sel)].find((e) => (e.textContent ?? "").includes(has));
        if (el) seen.push(what === "text" ? (el.textContent ?? "") : el.getBoundingClientRect().x);
      };
      new MutationObserver(read).observe(document.querySelector(root)!, { attributes: true, characterData: true, childList: true, subtree: true });
      return seen;
    },
    [root, sel, what, has] as const,
  );
}

test("a lane drag's overlay never shows its old span after the release; ⌘Z at once undoes the release", async ({ browser }) => {
  test.skip(!existsSync(REEL), "no test video on this machine");
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-races-")), "races-test.mp4");
  copyFileSync(REEL, copy);
  const { page, errors } = await open(browser, copy, "races-test");
  await openTab(page, "Short");
  await page.getByLabel("Hook text").fill("My PC made this");
  await page.getByLabel("Hook seconds").fill("3");
  await page.getByRole("button", { name: "Set hook" }).click();
  const tl = page.locator(".timeline-canvas");
  await expect(tl).toHaveAttribute("data-overlays-editable", "1", { timeout: 20_000 });
  const hookRow = page.getByRole("list", { name: "Overlays" }).getByRole("button").filter({ hasText: "text_hook" });
  await expect(hookRow).toContainText("frames 0–90, lane 1", { timeout: 20_000 });

  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const scroll = Number(await tl.getAttribute("data-scroll"));
  const ovY = Number((await tl.getAttribute("data-lanes-y"))!.split(",")[4]);
  const x = (f: number) => box.x + 12 + f * ppf - scroll;
  const y = box.y + ovY + 8;
  await page.keyboard.down("Alt");
  await page.mouse.move(x(45), y);
  await page.mouse.down();
  await page.mouse.move(x(105), y, { steps: 6 });
  await expect(tl).toHaveAttribute("data-dragging", "lane");
  const seen = await record(page, 'ol[aria-label="Overlays"]', "[data-overlay]", "text", "text_hook");
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(tl).toHaveAttribute("data-dragging", "", { timeout: 20_000 });
  await expect(hookRow).toContainText("frames 60–150");
  // the cut's next revision reads the lanes once more: give it the chance to draw
  await page.waitForTimeout(300);
  expect(await seen.evaluate((s) => s.filter((t) => String(t).includes("frames 0–90")))).toEqual([]);

  // ⌘Z at once after a release undoes that release (sent before the release's commit,
  // it undid the edit before it: here, the gain segment's add)
  const gainY = Number((await tl.getAttribute("data-lanes-y"))!.split(",")[2]);
  const yDb = (db: number) => box.y + gainY + 15 - (db / 24) * 13;
  const gainRows = page.getByRole("list", { name: "Gain" }).getByRole("button");
  await page.mouse.click(x(300), yDb(12));
  await expect(gainRows).toHaveCount(1, { timeout: 20_000 });
  await expect(gainRows.first()).toContainText("+12 dB (frames 285–315)");
  await page.keyboard.down("Alt");
  await page.mouse.move(x(300), yDb(12));
  await page.mouse.down();
  await page.mouse.move(x(330), yDb(6), { steps: 6 });
  // the release and ⌘Z in one task: the undo goes out before the release's commit can
  await page.evaluate(() => {
    document.querySelector(".timeline-canvas")!.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 1 }));
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "z", metaKey: true, ctrlKey: true, bubbles: true }));
  });
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(tl).toHaveAttribute("data-dragging", "", { timeout: 20_000 });
  await expect(gainRows).toHaveCount(1, { timeout: 20_000 });
  await expect(gainRows.first()).toContainText("+12 dB (frames 285–315)", { timeout: 20_000 });
  expect(errors).toEqual([]);
});

test("a crop box never jumps back after the release; ↓ ↓ at once jumps twice", async ({ browser }) => {
  test.skip(!existsSync(WIDE), "no test video on this machine");
  const { page, errors } = await open(browser, WIDE, /why-i-give/);
  const out = page.getByRole("group", { name: "Output" });
  await out.getByRole("button", { name: "9:16" }).click();
  await expect(out.getByRole("button", { name: "9:16" })).toHaveAttribute("aria-pressed", "true");
  await openTab(page, "Layout");
  await page.getByRole("button", { name: "Screen + facecam" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");
  await page.getByTestId("crop-mode").click();
  await expect(page.locator(".crop-box")).toHaveCount(2);

  // move the facecam box 60 px right; from the release on its left edge stays there
  const cam = page.locator(".crop-box[data-role=cam]");
  const c0 = (await cam.boundingBox())!;
  await page.mouse.move(c0.x + c0.width / 2, c0.y + c0.height / 2);
  await page.mouse.down();
  await page.mouse.move(c0.x + c0.width / 2 + 60, c0.y + c0.height / 2, { steps: 6 });
  const lefts = await record(page, "[data-testid=crop-overlay]", ".crop-box[data-role=cam]", "x");
  await page.mouse.up();
  await expect.poll(async () => (await cam.boundingBox())!.x, { timeout: 20_000 }).toBeGreaterThan(c0.x + 50);
  await page.waitForTimeout(300);
  expect(await lefts.evaluate((s, x0) => s.filter((x) => Number(x) < x0 + 30), c0.x)).toEqual([]);
  await page.getByTestId("crop-mode").click();

  // three parts (heads at frames 0, 50 and 100), the playhead at the start
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  for (const [n, tc] of [[2, /^0:01\.20/], [3, /^0:03\.10/]] as const) {
    for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowRight");
    await expect(page.getByTestId("timecode")).toHaveText(tc);
    await page.keyboard.press("s");
    await expect.poll(async () => (await page.getByTestId("cut-meta").textContent()) ?? "").toContain(`${n} parts`);
  }
  await page.keyboard.press("Home");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  // ↓ ↓ in one task (nothing from the viewer lands between them): part 3's head, not
  // part 2's twice
  await page.evaluate(() => {
    for (let i = 0; i < 2; i++) document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  });
  await expect(page.getByTestId("timecode")).toHaveText(/^0:03\.10/);
  expect(errors).toEqual([]);
});
