// Decision 0015 (V.01–V.03) and U.17 / R-UI-1 on a phone (WebKit, iPhone 15 profile,
// 390×844): two exports of one cut; the review link (dev builds make one without the
// sheets) lists them and loads, fits the width and takes its token out of the address bar;
// a comment with a box drawn on the frame shows as a pin and a marker; two versions compare
// side by side or A/B, kept on one clock; a note with markup saved there stays text on the
// phone and lands on the export's thread as the link's (R-UI-SEC-2); "Approve" on a loopback
// link waits for a click in the app (Home → Needs you), where it is dismissed.
import { devices, expect, test, webkit } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");
// Playwright's WebKit for Windows plays no H.264 (the review page's video); the phone check
// is Safari's, run on the Mac
test.skip(process.platform === "win32", "Playwright WebKit on Windows has no H.264 playback");

test("the review link on a phone: pins, compare, approve", async ({ browser }) => {
  test.setTimeout(900_000);
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
  const bridge = <T,>(c: string, a: Record<string, unknown> = {}) =>
    page.evaluate(([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<{ output: T }> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>), [c, a] as const);
  // two versions of the cut (two exports, two files)
  const pid = (await bridge<{ id: string }[]>("project_list_open")).output[0]!.id;
  for (let i = 1; i <= 2; i++) {
    await expect(page.getByTestId("export")).toBeEnabled({ timeout: 30_000 });
    await page.getByTestId("export").click();
    await expect
      .poll(async () => (await bridge<unknown[]>("exports_list", { project: pid })).output.length, { timeout: 360_000, intervals: [1000] })
      .toBe(i);
  }

  const link = (await bridge<{ url: string }>("dev_review_link")).output.url;
  const wk = await webkit.launch();
  const phone = await wk.newContext({ ...devices["iPhone 15"], viewport: { width: 390, height: 844 } });
  const m = await phone.newPage();
  const perrors: string[] = [];
  m.on("pageerror", (e) => perrors.push(String(e)));
  await m.goto(link);
  await expect(m.locator("#status")).toContainText("Project", { timeout: 20_000 });
  // the token left the address bar
  expect(m.url()).not.toContain("#t=");
  const card = m.locator(".cut-card").first();
  await expect(card.locator("h3").first()).toContainText("h79-v4");
  const vid = card.locator(".stage video");
  await expect.poll(async () => vid.evaluate((el) => (el as HTMLVideoElement).readyState), { timeout: 30_000 }).toBeGreaterThan(0);
  await expect(card).toContainText("2 versions");

  // a comment at 2.0 s with a box drawn on the frame
  await vid.evaluate((el) => { (el as HTMLVideoElement).currentTime = 2; });
  await card.getByRole("button", { name: "Draw a box" }).click();
  const pins = card.locator(".pins");
  await pins.scrollIntoViewIfNeeded();
  const b = (await pins.boundingBox())!;
  expect(b.y).toBeGreaterThanOrEqual(0);
  await m.mouse.move(b.x + b.width * 0.3, b.y + b.height * 0.3);
  await m.mouse.down();
  await m.mouse.move(b.x + b.width * 0.5, b.y + b.height * 0.4, { steps: 3 });
  await m.mouse.move(b.x + b.width * 0.7, b.y + b.height * 0.5, { steps: 3 });
  await m.mouse.up();
  await expect(card.locator(".pin.drawing")).toHaveCount(1);
  await card.getByLabel("Comment", { exact: true }).fill("this frame is blurry");
  await card.getByRole("button", { name: "Comment", exact: true }).click();
  await expect(card).toContainText("Comment added at 0:02.0");
  await expect(card.locator(".thread").filter({ hasText: "this frame is blurry" })).toBeVisible({ timeout: 20_000 });
  await expect(card.locator(".markers .marker")).toHaveCount(1);
  await vid.evaluate((el) => { (el as HTMLVideoElement).currentTime = 2.1; });
  await expect(card.locator(".pins .pin")).toHaveCount(1);
  // it is on the cut in the app, at frame 60, with its box
  await openTab(page, "Review");
  const t = page.getByTestId("comments-panel").getByTestId("thread").filter({ hasText: "this frame is blurry" });
  await expect(t.getByTestId("thread-time")).toHaveText("0:02.0", { timeout: 20_000 });
  await expect(t).toContainText("box");

  // compare two versions: A/B keeps the time; side by side keeps B on A's clock
  const cmp = card.locator(".compare");
  await expect(cmp).toBeVisible();
  const [va, vb] = [cmp.locator("video").nth(0), cmp.locator("video").nth(1)];
  await expect.poll(async () => vb.evaluate((el) => (el as HTMLVideoElement).readyState), { timeout: 30_000 }).toBeGreaterThan(0);
  await vb.evaluate((el) => { (el as HTMLVideoElement).currentTime = 1.5; });
  await cmp.getByRole("button", { name: "Show A" }).click();
  await expect.poll(async () => va.evaluate((el) => (el as HTMLVideoElement).currentTime)).toBeCloseTo(1.5, 1);
  await expect(va).not.toHaveClass(/hidden/);
  await expect(vb).toHaveClass(/hidden/);
  await cmp.getByRole("button", { name: "Side by side" }).click();
  const [sa, sb] = [cmp.locator("video").nth(0), cmp.locator("video").nth(1)];
  await expect.poll(async () => sa.evaluate((el) => (el as HTMLVideoElement).readyState), { timeout: 30_000 }).toBeGreaterThan(0);
  await sa.evaluate((el) => { (el as HTMLVideoElement).currentTime = 2.5; });
  await expect.poll(async () => sb.evaluate((el) => (el as HTMLVideoElement).currentTime), { timeout: 10_000 }).toBeCloseTo(2.5, 1);
  expect(await m.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);

  // a note with markup, from the phone: text after a reload
  const note = "<b>bold?</b> too jumpy";
  const notes = card.locator(".notes").first();
  await notes.locator("textarea").fill(note);
  await notes.locator("select").selectOption("change");
  await notes.getByRole("button", { name: "Save note" }).click();
  await expect(notes).toContainText("Saved.");
  await m.reload();
  await expect(m.locator(".untrusted").filter({ hasText: note }).first()).toBeVisible({ timeout: 20_000 });
  expect(await m.locator(".untrusted b").count()).toBe(0);

  // approve on a loopback link: waits for the app
  await card.getByRole("button", { name: "Approve → Clippy calendar" }).click();
  await expect(card).toContainText("Waiting for you in the app");
  await wk.close();
  await page.getByRole("button", { name: "Home" }).click();
  const ask = page.getByTestId("review-ask").first();
  await expect(ask).toContainText("send to Clippy", { timeout: 20_000 });
  await ask.getByTestId("review-ask-dismiss").click();
  await expect(page.getByTestId("review-ask")).toHaveCount(0);

  // the note is on the export's thread as the link's (not yours), its words as written
  type Thread = { tiles: { thread: { author: string; human: boolean; text: string }[] }[] };
  const thread = (await bridge<Thread>("board_view", { project: pid })).output.tiles.flatMap((t) => t.thread);
  const said = thread.find((m) => m.text.includes(note));
  expect(said?.human).toBe(false);
  expect(said?.author).toMatch(/^review-link:/);
  expect(perrors).toEqual([]);
  expect(errors).toEqual([]);
});
