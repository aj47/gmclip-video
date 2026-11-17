// U.09 remainder (SPEC §5.7): linked layouts (the part's layout picked, unlinked and
// linked again; each layer's crop and place say where they come from: the layout, this
// part, the framing's default), canvas snapping with guides (a layer dragged near the
// frame's edge snaps onto it; ⌥ drags freely), and the off-canvas ghost of a layer that
// reaches past the frame.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("linked layouts, snapping, off-canvas ghost", async ({ browser }) => {
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
  const out = page.getByRole("group", { name: "Output" });
  await out.getByRole("button", { name: "9:16" }).click();
  await expect(out.getByRole("button", { name: "9:16" })).toHaveAttribute("aria-pressed", "true");
  await openTab(page, "Layout");
  await page.getByRole("button", { name: "Screen + facecam" }).click();
  await expect(page.getByTestId("part-layout")).toHaveText("Screen + facecam 9:16");

  // the inspector: linked to that layout; both layers' values come from it
  await openTab(page, "Inspector");
  const pick = page.getByTestId("layout-link");
  await expect(pick.locator("option:checked")).toHaveText("Screen + facecam 9:16");
  const screen = page.getByRole("region", { name: "Layer screen" });
  if (!(await screen.getByRole("group", { name: "Crop (source pixels)" }).isVisible())) await screen.getByText("Numbers").click();
  await expect(screen.getByTestId("from-crop")).toHaveText("layout");
  await expect(screen.getByTestId("from-place")).toHaveText("layout");
  // this part's own crop: the tag says so
  const cropX = screen.getByRole("group", { name: "Crop (source pixels)" }).getByRole("spinbutton").first();
  const x0 = Number(await cropX.inputValue());
  await cropX.fill(String(x0 + 40));
  await cropX.press("Enter");
  await expect(screen.getByTestId("from-crop")).toHaveText("this part", { timeout: 20_000 });
  await expect(screen.getByTestId("from-place")).toHaveText("layout");
  // unlink: the framing's default stack; link again from the picker
  await page.getByTestId("layout-unlink").click();
  await expect(pick.locator("option:checked")).toHaveText("Not linked (default)", { timeout: 20_000 });
  await expect(page.getByRole("region", { name: /^Layer / }).first().getByTestId("from-place")).toHaveText("default");
  await pick.selectOption({ label: "Screen + facecam 9:16" });
  await expect(page.getByTestId("part-layout")).toHaveCount(0); // (the Layout tab is closed)
  await expect(screen.getByTestId("from-place")).toHaveText("layout", { timeout: 20_000 });

  // snapping: nudge the screen panel 4 px right: it snaps back onto the frame's left
  // edge with a guide; with ⌥ it moves freely and reaches past the right edge (ghost)
  const vb = page.locator(".viewer-box");
  const c = (await page.locator(".viewer-canvas").boundingBox())!;
  const k = 1080 / c.width;
  const py = c.y + (250 + 380) / k;
  await page.mouse.move(c.x + c.width / 2, py);
  await page.mouse.down();
  await page.mouse.move(c.x + c.width / 2 + 2, py, { steps: 2 });
  await page.mouse.move(c.x + c.width / 2 + 4, py, { steps: 2 });
  await expect(vb).not.toHaveAttribute("data-guides", "0");
  await expect(vb).toHaveAttribute("data-drag", /^0,250,1080,/);
  await page.keyboard.down("Alt");
  await page.mouse.move(c.x + c.width / 2 + 40, py, { steps: 4 });
  await page.mouse.move(c.x + c.width / 2 + 80, py, { steps: 4 });
  await expect(vb).toHaveAttribute("data-guides", "0");
  await expect(page.getByTestId("offcanvas-ghost")).toBeVisible();
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(vb).toHaveAttribute("data-drag", "");
  // placed past the edge: the ghost stays while it is selected; ⌘Z puts it back
  await expect(screen.getByRole("group", { name: "Place (canvas pixels)" }).getByRole("spinbutton").first()).not.toHaveValue("0", { timeout: 20_000 });
  await expect(page.getByTestId("offcanvas-ghost")).toBeVisible();
  await page.locator(".cut-head").click({ position: { x: 4, y: 4 } });
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.getByTestId("offcanvas-ghost")).toHaveCount(0, { timeout: 20_000 });
  expect(errors).toEqual([]);
});
