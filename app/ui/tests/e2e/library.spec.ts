// A2.16: Home speaks for the whole library. A project that is not open is listed under
// "In your library" (from the library index) and opens from there; its exports and staged
// moments reach Home too. Home's recent-video thumbnails: a video opened once shows a frame
// of itself on its Recent card (read from the file, or its scrub proxy once built), and the
// next visit reads it from the thumbnail cache.
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { STATE } from "./global-setup";
import { overflow, VIDEO } from "./ux-lib";

type Out = { ok?: boolean; output?: unknown };

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

test("a closed project is listed and opens from Home", async ({ browser }) => {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  const name = `Shelf ${Date.now()}`;
  const bridge = (c: string, a: Record<string, unknown>, project?: string) =>
    page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<Out> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
  // an empty project, made the way an agent does (Home has no New project since decision 0054)
  await bridge("project_new", { name });
  await page.reload();
  await expect(page.locator(".project").filter({ hasText: name })).toBeVisible();
  const list = (await bridge("project_list_open", {})).output as { id: string; name: string }[];
  const id = list.find((p) => p.name === name)!.id;
  await bridge("project_close", {}, id);
  await page.reload();
  const row = page.getByRole("region", { name: "Library" }).getByTestId("library-row").filter({ hasText: name });
  await expect(row).toBeVisible();
  await expect(row).toContainText("no exports");
  await row.getByRole("button", { name: "Open" }).click();
  await expect(page.locator(".project").filter({ hasText: name })).toBeVisible();
  await expect(page.getByTestId("library-row").filter({ hasText: name })).toHaveCount(0);
  expect(errors).toEqual([]);
});

// U.10 / A2.16: Home across the library. A closed stream's staged moment waits under
// Needs you (Grow, naming its project); every project's recent exports are listed (a
// closed one's opens its project); Grow opens the closed project and the moment's new cut;
// the moment then leaves Needs you. At 480×360 the rows fit the window.
test("Home lists every project's exports and staged moments; Grow opens a closed one", async ({ browser }) => {
  test.setTimeout(240_000);
  test.skip(!existsSync(VIDEO), "no test video on this machine");
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1100, height: 760 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  const stem = `shelf-stream-${Date.now()}`;
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-libhome-")), `${stem}.mp4`);
  copyFileSync(VIDEO, copy);
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText(stem, { timeout: 30_000 });
  const call = async <T,>(c: string, a: Record<string, unknown>, project?: string): Promise<T> => {
    const r = await page.evaluate(([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>, p?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>, p as string | undefined), [c, a, project] as const);
    if (!r.ok) throw new Error(`${c}: ${JSON.stringify(r.error)}`);
    return r.output as T;
  };
  const pid = (await call<{ id: string; name: string }[]>("project_list_open", {})).find((p) => p.name.includes(stem))!.id;
  const quote = "the run took sixty-seven seconds";
  const m = await call<{ id: string }>("moment_add", { start_s: 2, end_s: 7, quote, score: 4, type: "result", why: "library home" }, pid);
  await call("moment_set_state", { moment: m.id, state: "staged" }, pid);
  // the closed project's home gets an export (as another device would write it)
  // (the library index takes the new project in the background: under load it is a beat late)
  let home = "";
  await expect
    .poll(async () => (home = (await call<{ library: { id: string; path: string }[] }>("home", {})).library.find((l) => l.id === pid)?.path ?? ""), { timeout: 20_000 })
    .not.toBe("");
  await call("project_close", {}, pid);
  writeFileSync(join(home, "exports.json"), JSON.stringify([{ id: "x-shelf", fps: 30, cut: "c1", title: `${stem} final`, file: "/nowhere.mp4", frames: 300, seconds: 10, render_sig: null, at_ms: Date.now(), notes: [] }]));
  await page.getByRole("button", { name: "Home", exact: true }).click().catch(() => undefined);
  await page.reload();

  const needs = page.getByRole("region", { name: "Activity" }).getByTestId("needs-grow").filter({ hasText: quote });
  await expect(needs).toBeVisible({ timeout: 20_000 });
  await expect(needs).toContainText(stem);
  const recent = page.getByRole("region", { name: "Recent exports" }).getByTestId("recent-export").filter({ hasText: `${stem} final` });
  await expect(recent).toBeVisible();
  await expect(recent.getByRole("button", { name: "Open project" })).toBeVisible();
  await expect(page.getByTestId("library-row").filter({ hasText: stem })).toContainText("1 moment, 1 to grow");
  // small window: nothing runs off sideways
  await page.setViewportSize({ width: 480, height: 360 });
  await recent.scrollIntoViewIfNeeded();
  expect(await overflow(page)).toEqual([]);
  const rb = (await recent.boundingBox())!;
  expect(rb.x + rb.width).toBeLessThanOrEqual(481);
  await page.setViewportSize({ width: 1100, height: 760 });

  // Grow: the project opens, and the moment's new cut opens in the editor
  await needs.getByTestId("needs-grow-go").click();
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  await page.getByRole("button", { name: "Home", exact: true }).click();
  await expect(page.getByTestId("needs-grow").filter({ hasText: quote })).toHaveCount(0, { timeout: 20_000 });
  expect(errors).toEqual([]);
});

test("recent videos show a frame of themselves, cached", async ({ browser }) => {
  test.skip(!existsSync(VIDEO), "no test video on this machine");
  test.setTimeout(120_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-thumbs-")), "thumbs-test.mp4");
  copyFileSync(VIDEO, copy);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1100, height: 700 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("thumbs-test", { timeout: 30_000 });
  await page.getByRole("button", { name: "Home", exact: true }).click();
  const card = page.getByRole("button", { name: "Edit thumbs-test" });
  // the card names the folder the video is in (on Windows too, where paths use backslashes)
  await expect(card.locator(".recent-path")).toHaveText(basename(dirname(copy)));
  const thumb = card.getByTestId("recent-thumb");
  const t0 = Date.now();
  // the project map's Videos row asked for the same frame first (multi-video projects),
  // so it may already come from the cache
  await expect(thumb).toHaveAttribute("data-from", /^(file|proxy|cache)$/, { timeout: 20_000 });
  console.log(`first thumbnail: ${Date.now() - t0} ms (${await thumb.getAttribute("data-from")})`);
  await expect(thumb).toBeVisible();
  // a real frame: the canvas is the frame's shape and not one flat colour
  const stats = await thumb.evaluate((c: HTMLCanvasElement) => {
    const d = c.getContext("2d")!.getImageData(0, 0, c.width, c.height).data;
    let lo = 255;
    let hi = 0;
    for (let i = 0; i < d.length; i += 4) {
      const y = (d[i]! + d[i + 1]! + d[i + 2]!) / 3;
      lo = Math.min(lo, y);
      hi = Math.max(hi, y);
    }
    return { w: c.width, h: c.height, spread: hi - lo };
  });
  // a portrait reel: 9:16 inside the card's 320 × 180 box
  expect(stats.h).toBeGreaterThan(175);
  expect(stats.h).toBeLessThanOrEqual(180);
  expect(Math.abs(stats.w / stats.h - 9 / 16)).toBeLessThan(0.02);
  expect(stats.spread).toBeGreaterThan(40);
  // again: from the cache
  await page.reload();
  const t1 = Date.now();
  await expect(page.getByRole("button", { name: "Edit thumbs-test" }).getByTestId("recent-thumb")).toHaveAttribute("data-from", "cache", { timeout: 20_000 });
  console.log(`cached thumbnail: ${Date.now() - t1} ms`);
  expect(errors).toEqual([]);
});
