// Decision 0042: a video dropped on the timeline becomes a picture layer at the frame
// under the drop; it goes behind the picture's layers and its person is cut out, each one
// command, and the lane says so.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { bridge } from "./qa-lib";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("drop a video on the timeline, put it behind the picture, cut its person out", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-booted", "1", { timeout: 60_000 });
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await openTab(page, "Inspector");
  const tl = page.locator(".timeline-canvas");
  await expect.poll(async () => Number(await tl.getAttribute("data-frames"))).toBeGreaterThan(0);

  // a copy dropped a third of the way along the timeline
  const broll = join(mkdtempSync(join(tmpdir(), "gmclip-layers-")), "broll.mp4");
  copyFileSync(VIDEO, broll);
  const box = (await tl.boundingBox())!;
  const ppf = Number(await tl.getAttribute("data-ppf"));
  const x = box.x + box.width / 3;
  const want = Math.round((x - box.x - 12) / ppf);
  await bridge(page, "files_dropped", { paths: [broll], at: [x, box.y + box.height / 2] });
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video", { timeout: 60_000 });
  // its own row in the timeline's layers, its clip where it was dropped
  const tracks = page.getByTestId("layer-tracks");
  const clip = tracks.getByTestId("layer-track-clip");
  await expect(clip).toBeVisible();
  // (the timeline may narrow as the rows arrive: read its mapping again)
  await expect.poll(async () => {
    const b2 = (await tl.boundingBox())!;
    const p2 = Number(await tl.getAttribute("data-ppf"));
    const s2 = Number(await tl.getAttribute("data-scroll"));
    return Math.abs((await clip.boundingBox())!.x - (b2.x + 12 + want * p2 - s2));
  }, { message: "starts at the drop" }).toBeLessThanOrEqual(3);
  // in front of the picture: the top row
  await expect(tracks).toHaveAttribute("data-order", /^overlay:/);
  // not offered as a new video of the project
  await expect(page.getByRole("dialog", { name: "Import videos" })).toHaveCount(0);

  // on the preview at its start: its tag drags it (the box lets clicks through)
  await page.getByTestId("layer-track-clip").click();
  const moveTag = page.getByTestId("gizmo-picture-move");
  await expect(moveTag).toBeVisible({ timeout: 20_000 });
  const row0 = page.locator('[data-testid="overlay-row"][data-kind="video"]');
  const px = row0.getByRole("group", { name: "Place" }).getByLabel("x");
  const x0 = Number(await px.getAttribute("data-value"));
  const tb = (await moveTag.boundingBox())!;
  await page.mouse.move(tb.x + tb.width / 2, tb.y + tb.height / 2);
  await page.mouse.down();
  await page.mouse.move(tb.x + tb.width / 2 + 40, tb.y + tb.height / 2 + 30, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => Number(await px.getAttribute("data-value")), { timeout: 20_000 }).toBeGreaterThan(x0);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("Meta+z");
  await expect.poll(async () => Number(await px.getAttribute("data-value")), { timeout: 20_000 }).toBe(x0);
  // picked on the timeline: its whole box drags on the preview; Esc lets go
  await page.getByTestId("layer-track-clip").click();
  const pic = page.getByTestId("gizmo-picture");
  await expect(pic).toHaveClass(/\bsel\b/);
  const pb = (await pic.boundingBox())!;
  await page.mouse.move(pb.x + pb.width / 2, pb.y + pb.height / 2);
  await page.mouse.down();
  await page.mouse.move(pb.x + pb.width / 2 - 50, pb.y + pb.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => Number(await px.getAttribute("data-value")), { timeout: 20_000 }).toBeLessThan(x0);
  await page.keyboard.press("Escape");
  await expect(pic).not.toHaveClass(/\bsel\b/);
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("Meta+z");
  await expect.poll(async () => Number(await px.getAttribute("data-value")), { timeout: 20_000 }).toBe(x0);

  // behind the whole picture, then its person cut out
  const row = page.locator('[data-testid="overlay-row"][data-kind="video"]');
  await expect(row).toHaveCount(1, { timeout: 20_000 });
  const stack = row.getByTestId("overlay-stack");
  await expect(stack).toHaveValue("");
  const behind = await stack.locator("option").nth(1).getAttribute("value");
  expect(behind).not.toBeNull();
  await stack.selectOption(behind!);
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video:under", { timeout: 20_000 });
  // behind the whole picture: the bottom row
  await expect(tracks).toHaveAttribute("data-order", /,overlay:[^,]+$/);
  await row.getByTestId("overlay-matte").check();
  await expect(row.getByTestId("overlay-matte")).toHaveAttribute("data-on", "1", { timeout: 20_000 });
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video:under:matte", { timeout: 20_000 });

  // dragged back to the top row by its grip: in front again
  const grip = tracks.locator('[data-key^="overlay:"]').getByTestId("layer-track-grip");
  const gb = (await grip.boundingBox())!;
  await page.mouse.move(gb.x + gb.width / 2, gb.y + gb.height / 2);
  await page.mouse.down();
  await page.mouse.move(gb.x + gb.width / 2, gb.y - 200, { steps: 6 });
  await page.mouse.up();
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video:matte", { timeout: 20_000 });
  await expect(tracks).toHaveAttribute("data-order", /^overlay:/);
  await stack.selectOption(behind!);
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video:under:matte", { timeout: 20_000 });

  // one undo at a time: in front again, then not cut out
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("Meta+z");
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video:matte", { timeout: 20_000 });
  await page.keyboard.press("Meta+z");
  await page.keyboard.press("Meta+z");
  await expect(tl).toHaveAttribute("data-overlay-kinds", "video:under", { timeout: 20_000 });

  // a double-click on a layer's clip edits its crop on the source; Esc is done
  await tracks.getByTestId("layer-track-layer-clip").first().dblclick();
  await expect(page.locator(".crop-box").first()).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("crop-exit")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".crop-box")).toHaveCount(0);
  // and on the preview itself
  await page.locator(".viewer-canvas").dblclick();
  await expect(page.locator(".crop-box").first()).toBeVisible({ timeout: 20_000 });
  await page.getByTestId("crop-exit").click();
  await expect(page.locator(".crop-box")).toHaveCount(0);
  expect(errors).toEqual([]);
});
