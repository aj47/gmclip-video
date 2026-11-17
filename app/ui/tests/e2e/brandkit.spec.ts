// The brand kit: name, handle, face, colours, logo and caption style saved (project + my
// kit); a lower third and a loop end card from it; a callout saved as a template and
// inserted; and a new video's project starting in my kit. First (QA, milestones/m2/
// qa-e2e.md, W6), on a fresh library: a kit saved with a name and handle only (the face
// left as it was) still makes a lower third and an end card. It used to save with no
// face, enable both buttons, and then refuse them ("the brand kit needs a heading face").
import { expect, test } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const VIDEO = labMedia("renders/own-vm-1080.mp4");
const VIDEO2 = labMedia("h79/h79-v4.mp4");
// a 2×2 PNG (red, with one transparent pixel)
const LOGO = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAE0lEQVR4nGP4z8DwH4QZwAQQAABCzwX7egQgQwAAAABJRU5ErkJggg==",
  "base64",
);

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

// before the full kit below: no kit exists yet (a copy of the reel, so the new project
// the kit test opens later is still new)
test("a kit saved without picking a face makes a lower third and an end card", async ({ browser }) => {
  test.skip(!existsSync(VIDEO2), "no second test video on this machine");
  test.setTimeout(180_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-kit-")), "kit-face.mp4");
  copyFileSync(VIDEO2, copy);
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
  await expect(page.getByTestId("cut-title")).toHaveText("kit-face", { timeout: 30_000 });
  await openTab(page, "Inspector");
  await page.getByTestId("brand-toggle").click();
  await page.getByTestId("brand-name").fill("TechFren");
  await page.getByTestId("brand-handle").fill("@techfren");
  await page.getByTestId("brand-save").click();
  await expect(page.getByTestId("lower-third")).toBeEnabled({ timeout: 20_000 });
  const rows = page.getByTestId("overlay-row");
  await page.getByTestId("lower-third").click();
  await expect(rows.filter({ hasText: "Lower third" })).toHaveCount(1, { timeout: 20_000 });
  await page.getByTestId("end-card").click();
  await expect(rows.filter({ hasText: "End card" })).toHaveCount(1, { timeout: 20_000 });
  await expect(page.locator(".toast.error")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("brand kit: save, lower third, end card, template, new project", async ({ browser }) => {
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
  await page.getByRole("group", { name: "Output" }).getByRole("button", { name: "9:16" }).click();
  await openTab(page, "Inspector");

  // The kit.
  await page.getByTestId("brand-toggle").click();
  await page.getByTestId("brand-name").fill("TechFren");
  await page.getByTestId("brand-handle").fill("@techfren");
  await page.getByTestId("brand-face").selectOption("Figtree Black");
  await page.getByTestId("brand-colour-accent").fill("#3ddc84");
  await page.getByTestId("brand-preset").selectOption("bold_pop");
  await page.getByTestId("brand-save").click();
  await expect(page.getByTestId("lower-third")).toBeEnabled({ timeout: 20_000 });
  await page.getByTestId("brand-logo").setInputFiles({ name: "logo.png", mimeType: "image/png", buffer: LOGO });
  await expect(page.getByRole("region", { name: "Brand kit" }).locator(".ov-thumb")).toHaveCount(1, { timeout: 20_000 });

  // A lower third (text + logo) and an end card ending on the opening layout.
  await page.getByTestId("lower-third").click();
  const rows = page.getByTestId("overlay-row");
  await expect(rows.filter({ hasText: "Lower third" })).toHaveCount(1, { timeout: 20_000 });
  await expect(rows.filter({ hasText: "Image" })).toHaveCount(1);
  if (process.env.GMCLIP_SHOT) {
    await page.waitForTimeout(1500);
    await page.getByRole("region", { name: "Viewer" }).screenshot({ path: process.env.GMCLIP_SHOT });
    await page.getByRole("region", { name: "Brand kit" }).screenshot({ path: process.env.GMCLIP_SHOT.replace(".png", "-panel.png") });
  }
  await page.getByTestId("end-card").click();
  await expect(rows.filter({ hasText: "End card" })).toHaveCount(1, { timeout: 20_000 });

  // A callout saved as a template, inserted at the playhead.
  await page.getByTestId("callout-box").click();
  const box = page.locator('[data-testid="overlay-row"][data-kind="box"]');
  await expect(box).toHaveCount(1, { timeout: 20_000 });
  await box.getByTestId("template-save").click();
  await page.getByTestId("template-name").fill("Red box");
  await page.getByTestId("template-name-save").click();
  await expect(page.locator('[data-testid="template-insert"][data-name="Red box"]')).toHaveCount(1, { timeout: 20_000 });
  await page.locator('[data-testid="template-insert"][data-name="Red box"]').click();
  await expect(page.locator('[data-testid="overlay-row"][data-kind="box"]')).toHaveCount(2, { timeout: 20_000 });

  // A new video's project starts in my kit (name, accent, the template).
  test.skip(!existsSync(VIDEO2), "no second test video on this machine");
  await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Home" }).click();
  await page.getByLabel("Video path").fill(VIDEO2);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("h79", { timeout: 30_000 });
  await openTab(page, "Inspector");
  await page.getByTestId("brand-toggle").click();
  await expect(page.getByTestId("brand-name")).toHaveValue("TechFren", { timeout: 20_000 });
  await expect(page.getByTestId("brand-colour-accent")).toHaveValue("#3ddc84");
  await expect(page.getByTestId("brand-preset")).toHaveValue("bold_pop");
  await expect(page.getByTestId("brand-face")).toHaveValue("Figtree Black");
  await expect(page.locator('[data-testid="template-insert"][data-name="Red box"]')).toHaveCount(1);
  expect(errors).toEqual([]);
});
