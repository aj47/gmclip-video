// R-UI-1 [M2] reachability smoke at 1100×700 and 1440×900, through the vertical slice:
// a project with an RT cut imported (as an agent does) → the viewer shows frames → trim parts on the timeline
// (an out edge and the next part's in edge) and from the inspector → move a layer →
// export starts with progress → the export popover shows the file.
// One page for the whole run: the frame socket accepts one connection per launch.
import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labPlan } from "./lab";
import { openTab } from "./tabs";

const H79 = labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json"));
const VIEWPORTS = [
  { width: 1100, height: 700 },
  { width: 1440, height: 900 },
];

test.describe.configure({ mode: "serial" });
// S2a runs alone (it takes the one frame socket of its launch).
test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

let page: Page;
const consoleErrors: string[] = [];

test.beforeAll(async ({ browser }) => {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  page = await browser.newPage({ viewport: VIEWPORTS[1]! });
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
});

async function inViewport(sel: string) {
  const box = await page.locator(sel).first().boundingBox();
  const vp = page.viewportSize()!;
  expect(box, `${sel} has a box`).not.toBeNull();
  expect(box!.width).toBeGreaterThan(20);
  expect(box!.height).toBeGreaterThan(10);
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(vp.width + 1);
  expect(box!.y + box!.height).toBeLessThanOrEqual(vp.height + 1);
}

/** Parts as the accessible mirror lists them: [out start, out end]. */
async function parts(): Promise<[number, number][]> {
  const texts = await page.getByRole("list", { name: "Parts" }).getByRole("button").allTextContents();
  return texts.map((t) => {
    const m = /frames (\d+)–(\d+)/.exec(t)!;
    return [Number(m[1]), Number(m[2])];
  });
}

/** Drags the timeline at output frame `at` by `frames` frames (side: which side of a boundary). */
async function dragEdge(at: number, frames: number, side: -1 | 1) {
  const c = page.locator(".timeline-canvas");
  const box = (await c.boundingBox())!;
  const ppf = Number(await c.getAttribute("data-ppf"));
  const scroll = Number(await c.getAttribute("data-scroll"));
  const x = box.x + 12 + at * ppf - scroll + side * 2;
  const y = box.y + 28 + 30;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + frames * ppf, y, { steps: 6 });
  await page.mouse.up();
}

test("Trusted Types are enforced and the page loads clean", async () => {
  await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.dataset.tt)).toBe("enforced");
  const refused = await page.evaluate(() => {
    try {
      // eslint-disable-next-line no-restricted-syntax, no-restricted-properties -- the test proves the sink is closed
      document.createElement("div").innerHTML = "<img src=x onerror=alert(1)>";
      return false;
    } catch {
      return true;
    }
  });
  expect(refused).toBe(true);
});

test("Home reachable at both sizes; a new project; an RT import opens the cut", async () => {
  for (const vp of VIEWPORTS) {
    await page.setViewportSize(vp);
    await expect(page.getByRole("main", { name: "Home" })).toBeVisible();
    for (const sel of ["[aria-label='Open a video']", "nav[aria-label=Main]"]) await inViewport(sel);
    // an empty library has nothing said to search: the box waits for the first video
    await expect(page.getByTestId("home-search")).toHaveCount(0);
    // nothing running: no Activity row for it
    await expect(page.locator("[aria-label=Running]")).toHaveCount(0);
  }
  await importPlanProject(page, "Playwright", H79);
  await expect(page.getByTestId("cut-title")).toHaveText("My PC made this in 67 seconds", { timeout: 60_000 });
});

for (const vp of VIEWPORTS) {
  test(`cut view reachable at ${vp.width}×${vp.height}`, async () => {
    await page.setViewportSize(vp);
    await page.waitForTimeout(300);
    // The side panel opens on the transcript; the inspector is a tab beside it.
    await openTab(page, "Inspector");
    // (the words strip shows only when the cut has words; this imported plan has none)
    for (const sel of [".viewer-canvas", ".transport", "[data-testid=look-pane]", ".timeline-canvas", "[data-testid=export]", "nav[aria-label=Main]"]) {
      await inViewport(sel);
    }
    await expect(page.getByRole("button", { name: /^(Play|Pause)$/ })).toBeVisible();
    // Frames arrive over the frame socket and are drawn at the displayed size.
    await expect(page.getByTestId("viewer-stats")).toContainText("×", { timeout: 30_000 });
    await expect(page.locator(".words")).toHaveCount(0);
    await expect(page.getByTestId("insp-part").getByRole("heading", { name: /^Part / })).toBeVisible();
  });
}

test("play advances the playhead", async () => {
  const tc = page.getByTestId("timecode");
  const before = await tc.textContent();
  await page.getByRole("button", { name: "Play", exact: true }).click();
  await page.waitForTimeout(1500);
  await page.getByRole("button", { name: "Pause", exact: true }).click();
  expect(await tc.textContent()).not.toBe(before);
});

test("trim on the timeline: part 1's out edge, then part 2's in edge", async () => {
  const p0 = await parts();
  const total = Number(await page.locator(".timeline-canvas").getAttribute("data-frames"));
  expect(total).toBe(p0[p0.length - 1]![1]);
  // Part 1 out edge 12 frames earlier (pointer on its left side of the boundary).
  await dragEdge(p0[0]![1], -12, -1);
  await expect.poll(async () => (await parts())[0]![1], { timeout: 20_000 }).toBe(p0[0]![1] - 12);
  const p1 = await parts();
  expect(p1[1]![1] - p1[1]![0]).toBe(p0[1]![1] - p0[1]![0]);
  // Part 2 in edge 6 frames later (pointer on the right side): part 2 shortens by 6.
  await dragEdge(p1[1]![0], 6, 1);
  await expect.poll(async () => { const p = await parts(); return p[1]![1] - p[1]![0]; }, { timeout: 20_000 })
    .toBe(p1[1]![1] - p1[1]![0] - 6);
  expect((await parts())[0]).toEqual(p1[0]);
});

test("trim from the inspector by keyboard; move a layer by number", async () => {
  // The list mirror is keyboard-only: focus it and press Enter.
  const third = page.getByRole("list", { name: "Parts" }).getByRole("button").nth(2);
  await third.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("insp-part").getByRole("heading", { name: "Part 3" })).toBeVisible();
  const before = (await parts())[2]!;
  if (!(await page.getByRole("button", { name: "Move the out point one frame earlier" }).isVisible())) await page.getByText("Trim by frames and hold").click();
  await page.getByRole("button", { name: "Move the out point one frame earlier" }).click();
  await expect.poll(async () => (await parts())[2]![1], { timeout: 20_000 }).toBe(before[1] - 1);
  const layer = page.getByRole("region", { name: "Layer screen" }).first();
  // the numbers are folded under the layer's summary
  const x = layer.getByRole("group", { name: "Place (canvas pixels)" }).getByLabel("x");
  if (!(await x.isVisible())) await layer.getByText("Numbers").click();
  const old = Number(await x.inputValue());
  await x.fill(String(old + 20));
  await x.press("Enter");
  await expect.poll(async () => Number(await x.inputValue()), { timeout: 20_000 }).toBe(old + 20);
  await expect(page.getByTestId("saved")).toHaveText("Saved");
});

test("export starts with progress; the review board lists exports", async () => {
  if (process.env.GMCLIP_E2E_FULL_EXPORT) test.setTimeout(900_000);
  await page.getByTestId("export").click();
  await expect(page.getByTestId("export-progress")).toBeVisible({ timeout: 20_000 });
  await expect
    .poll(async () => Number(await page.locator("[data-testid=export-progress] progress").getAttribute("value")), { timeout: 180_000 })
    .toBeGreaterThan(0);
  if (!process.env.GMCLIP_E2E_FULL_EXPORT) {
    await page.getByRole("button", { name: "Cancel" }).click();
    await expect(page.getByTestId("export")).toBeVisible({ timeout: 30_000 });
  } else {
    await expect(page.locator(".export-state")).toContainText("Exported", { timeout: 600_000 });
  }
  for (const vp of VIEWPORTS) {
    await page.setViewportSize(vp);
    await page.getByRole("button", { name: "Export options" }).click();
    await inViewport(".export-split .popover");
    if (process.env.GMCLIP_E2E_FULL_EXPORT) await inViewport("[data-testid=export-reveal]");
    else await expect(page.getByTestId("export-file")).toHaveCount(0);
    await page.getByRole("button", { name: "Export options" }).click();
  }
});

test("no console errors (CSP, Trusted Types, IPC)", async () => {
  expect(consoleErrors.filter((e) => !/Failed to load resource/.test(e))).toEqual([]);
});
