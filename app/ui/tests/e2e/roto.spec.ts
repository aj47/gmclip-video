// R.01–R.03 in the app: on an opened video in 9:16 with Screen + facecam, cut the person
// out of the cam (no green screen), see the draft-matte badge, the accurate matte building
// on its own with ticks on the timeline, the matte view, a brush fix
// on the part (undoable), the quality list, and "Follow the action" on the screen.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("cut-out, brush, quality, follow the action", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  // the app's CSP refuses inline style attributes (a static style={{…}} becomes one)
  page.on("console", (m) => {
    if (m.type() === "error" && /Content Security Policy/.test(m.text())) errors.push(m.text());
  });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Layout");
  await page.getByRole("button", { name: "Screen + facecam" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");
  await openTab(page, "Inspector");
  const roto = page.getByTestId("roto-panel");

  // Windows has no person segmentation (it is Apple Vision): the cut-out is off there and
  // says so; the follow-the-action crop below works everywhere
  if (process.platform === "win32") {
    await expect(roto.getByTestId("roto-cam")).toBeDisabled();
    await expect(roto.getByTestId("roto-unavailable")).toBeVisible();
  } else {
    // Cut the person out of the cam: a draft matte until the accurate one is built.
    await roto.getByTestId("roto-cam").check();
    await expect(roto.getByTestId("roto-cam")).toBeChecked({ timeout: 20_000 });
    await expect(page.getByTestId("viewer-draft-matte")).toBeVisible({ timeout: 30_000 });
    await expect(roto.getByText("0 fix(es)")).toBeVisible();

    // R.01: the accurate matte builds on its own (no button), with ticks on the timeline.
    const tl = page.locator("canvas.timeline-canvas");
    await expect(tl).toHaveAttribute("data-matte-build", "running auto", { timeout: 20_000 });
    const built = async () =>
      ((await tl.getAttribute("data-matte-ticks")) ?? "").split(",").filter(Boolean).reduce((n, t) => n + Number(t.split("/")[0]), 0);
    const first = await built();
    await expect.poll(built, { timeout: 20_000 }).toBeGreaterThan(first);

    // Matte view on and off.
    await roto.getByTestId("matte-view").check();
    await expect(roto.getByTestId("matte-view")).toBeChecked({ timeout: 10_000 });
    await roto.getByTestId("matte-view").uncheck();

    // Brush: a stroke across the cam panel becomes one fix on this part; ⌘Z takes it back.
    await roto.getByTestId("brush").check();
    const ov = page.getByTestId("brush-overlay");
    await expect(ov).toBeVisible();
    const b = (await ov.boundingBox())!;
    await page.mouse.move(b.x + b.width * 0.4, b.y + b.height * 0.75);
    await page.mouse.down();
    await page.mouse.move(b.x + b.width * 0.5, b.y + b.height * 0.75, { steps: 5 });
    await page.mouse.move(b.x + b.width * 0.6, b.y + b.height * 0.78, { steps: 5 });
    await page.mouse.up();
    await expect(roto.getByText("1 fix(es)")).toBeVisible({ timeout: 20_000 });
    await roto.getByTestId("brush").uncheck();
    await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
    await page.keyboard.press("ControlOrMeta+z");
    await expect(roto.getByText("0 fix(es)")).toBeVisible({ timeout: 20_000 });

    // Quality: the cam part listed (not built yet).
    await roto.getByTestId("matte-quality").click();
    await expect(roto.getByTestId("matte-rows")).toContainText("not built yet", { timeout: 30_000 });
  }

  // Follow the action on the screen layer of this part.
  await roto.getByTestId("follow-part").click();
  await expect(roto.getByTestId("follow-result")).toBeVisible({ timeout: 60_000 });

  expect(errors).toEqual([]);
});
