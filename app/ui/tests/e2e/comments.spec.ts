// Decision 0015 (V.01, V.02, V.04, V.05) on the desktop: a comment at the playhead with a
// box drawn on the frame shows as a pin on the viewer and a marker on the timeline; reply,
// resolve; an editor's review link (a second page) comments with markup and suggests a
// strike, which shows in the app as text and applies only on Accept (⌘Z undoes it); the
// golden switch; "Fix all with my agent" with the zero-spend stub model leaves an answer
// in the thread. Then (QA, milestones/m2/qa-e2e.md, W4) a reply being typed survives the
// panel's refresh (every 5 s it looks again for review-link notes, and after every edit):
// the draft, the input and its focus stay; it was wiped, so a reply taking more than a few
// seconds to type could never be sent.
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("h79/h79-v4.mp4");
const hasClaude = (() => {
  try {
    return /^2\.1\.\d+ /.test(execFileSync("claude", ["--version"], { encoding: "utf8" }));
  } catch {
    return false;
  }
})();

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

async function dragOn(page: Page, sel: string, from: [number, number], to: [number, number]) {
  const b = (await page.locator(sel).boundingBox())!;
  await page.mouse.move(b.x + b.width * from[0], b.y + b.height * from[1]);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width * ((from[0] + to[0]) / 2), b.y + b.height * ((from[1] + to[1]) / 2), { steps: 4 });
  await page.mouse.move(b.x + b.width * to[0], b.y + b.height * to[1], { steps: 4 });
  await page.mouse.up();
}

test("comments, a box on the frame, an editor's suggestion, golden, fix with my agent", async ({ browser }) => {
  test.setTimeout(600_000);
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

  // a comment 3 s in, with a box drawn on the frame
  await openTab(page, "Review");
  const panel = page.getByTestId("comments-panel");
  await expect(panel).toBeVisible();
  const bridge = <T,>(c: string, a: Record<string, unknown> = {}) =>
    page.evaluate(([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<{ output: T }> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>), [c, a] as const);
  await bridge("viewer_seek", { frame: 90 });
  await expect(page.getByTestId("comment-text")).toHaveAttribute("placeholder", "Comment at 0:03.0");
  await page.getByTestId("comment-text").fill("The prompt is unreadable here");
  await page.getByTestId("draw-box").click();
  await dragOn(page, '[data-testid="comment-pins"]', [0.2, 0.2], [0.7, 0.35]);
  await expect(page.getByTestId("draft-box")).toBeVisible();
  await page.getByTestId("comment-add").click();
  const thread = panel.getByTestId("thread").first();
  await expect(thread).toContainText("The prompt is unreadable here");
  await expect(thread).toContainText("box");
  await expect(thread.getByTestId("thread-time")).toHaveText("0:03.0");
  // the pin on the viewer (near the playhead) and the marker on the timeline
  await expect(page.locator('[data-testid="comment-pins"] .comment-pin')).toHaveCount(1);
  await expect(page.locator(".timeline-canvas")).toHaveAttribute("data-comments", "90");
  // reply, resolve, show resolved
  await thread.getByRole("textbox", { name: "Reply" }).fill("on it");
  await thread.getByRole("button", { name: "Send reply" }).click();
  await expect(thread.getByTestId("thread-reply")).toContainText("on it");
  await thread.getByTestId("thread-resolve").click();
  await expect(panel.getByTestId("thread")).toHaveCount(0);
  await panel.getByRole("button", { name: "Show 1 resolved" }).click();
  await expect(panel.getByTestId("thread")).toHaveCount(1);

  // an export, so the review link has something to play
  await page.getByTestId("export").click();
  await expect(page.getByTestId("export-done")).toBeVisible({ timeout: 360_000 });

  // golden: a finished cut can be marked
  await panel.getByTestId("golden-switch").check();
  await expect(panel.getByTestId("golden-switch")).toBeChecked();

  // an editor's review link on a second page: a comment with markup and a strike
  const link = (await bridge<{ url: string }>("dev_review_link", { role: "editor" })).output.url;
  const ed = await browser.newPage({ viewport: { width: 390, height: 844 } });
  await ed.goto(link);
  await expect(ed.locator("#status")).toContainText("editor link", { timeout: 20_000 });
  const v = ed.locator(".cut-card video").first();
  await expect.poll(async () => v.evaluate((el) => (el as HTMLVideoElement).readyState), { timeout: 20_000 }).toBeGreaterThan(0);
  const xss = '<img src=x onerror="window.__xss=1"> slow open';
  await v.evaluate((el) => { (el as HTMLVideoElement).currentTime = 1; });
  await ed.getByLabel("Comment", { exact: true }).fill(xss);
  const loads = Number((await ed.locator("body").getAttribute("data-loads")) ?? 0);
  // the reload it starts is slow here, as on a loaded Mac (the race happens every time)
  await ed.route("**/api/home", async (r) => {
    await new Promise((d) => setTimeout(d, 1500));
    await r.continue();
  });
  await ed.getByRole("button", { name: "Comment", exact: true }).click();
  await expect(ed.locator(".cut-card")).toContainText("Comment added at 0:01.0");
  // the comment reloads the page's cards: the form below is drawn again, so pick in the
  // new one (under load the reload landed after the pick and took "From here" away)
  await expect.poll(async () => Number((await ed.locator("body").getAttribute("data-loads")) ?? 0), { timeout: 60_000 }).toBeGreaterThan(loads);
  await ed.unroute("**/api/home");
  await ed.getByLabel("Kind of edit").selectOption("strike");
  await v.evaluate((el) => { (el as HTMLVideoElement).currentTime = 4; });
  await ed.getByRole("button", { name: "From here" }).click();
  await v.evaluate((el) => { (el as HTMLVideoElement).currentTime = 4.5; });
  await ed.getByRole("button", { name: "To here" }).click();
  await ed.getByLabel("Why").fill("dead air");
  await ed.getByRole("button", { name: "Suggest", exact: true }).click();
  await expect(ed.locator(".suggest")).toContainText("waits for the owner");
  expect(await ed.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await ed.close();

  // back in the app: the link's comment is text, the suggestion waits; Accept, then ⌘Z
  await expect(panel.getByTestId("thread").filter({ hasText: "slow open" })).toBeVisible({ timeout: 20_000 });
  await expect(panel.locator(".thread img")).toHaveCount(0);
  expect(await page.evaluate(() => (globalThis as unknown as { __xss?: number }).__xss)).toBeUndefined();
  const sug = panel.getByTestId("suggestion").first();
  await expect(sug).toContainText("Strike 0.50 s");
  await expect(sug).toContainText("dead air");
  const seconds = async () => Number(await page.getByTestId("cut-meta").getAttribute("data-seconds"));
  const before = await seconds();
  await sug.getByTestId("suggestion-accept").click();
  await expect(sug).toHaveAttribute("data-state", "accepted");
  await expect.poll(seconds).toBeLessThan(before - 0.4);
  await page.locator("body").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(seconds).toBeCloseTo(before, 1);
  await expect(panel.getByTestId("suggestion").first()).toHaveAttribute("data-state", "proposed");

  // "Fix all with my agent": the stub model's run ends; every task has an answer
  if (hasClaude) {
    await panel.getByTestId("fix-all").click();
    const open = panel.getByTestId("thread").filter({ hasText: "slow open" });
    await expect(open.getByTestId("thread-reply")).toContainText("Your agent", { timeout: 240_000 });
  }
  expect(errors).toEqual([]);
});

// a copy of the reel: its own project (the test above leaves threads and an agent's title)
test("a reply draft survives the panel's refresh", async ({ browser }) => {
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-draft-")), "comments-draft.mp4");
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
  await expect(page.getByTestId("cut-title")).toHaveText("comments-draft", { timeout: 30_000 });
  await openTab(page, "Review");
  const panel = page.getByTestId("comments-panel");
  for (const text of ["first note", "second note"]) {
    await page.getByTestId("comment-text").fill(text);
    await page.getByTestId("comment-add").click();
    await expect(panel.getByTestId("thread").filter({ hasText: text })).toBeVisible();
  }
  const thread = panel.getByTestId("thread").filter({ hasText: "first note" });
  const box = thread.getByRole("textbox", { name: "Reply" });
  await box.click();
  await box.pressSequentially("half a rep");
  // the panel looks again every 5 s; another edit refreshes it too
  await page.waitForTimeout(6_000);
  await page.getByTestId("comment-text").fill("third note");
  await page.getByTestId("comment-add").click();
  await expect(panel.getByTestId("thread").filter({ hasText: "third note" })).toBeVisible();
  await expect(box).toHaveValue("half a rep");
  await box.press("End");
  await box.pressSequentially("ly");
  await thread.getByRole("button", { name: "Send reply" }).click();
  await expect(thread.getByTestId("thread-reply")).toContainText("half a reply");
  expect(errors).toEqual([]);
});
