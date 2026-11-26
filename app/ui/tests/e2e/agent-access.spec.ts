// Decision 0057: Settings › Agent access lists the human's home folder as readable by
// default, the folder field takes `~` and `~/…` and shows the absolute path it adds, and
// an Owner change from the dev bridge is still refused (no sheet, never).
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { STATE } from "./global-setup";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

test("the home folder is readable and ~ expands", async ({ browser }) => {
  test.setTimeout(90_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");

  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByTestId("agent-access").click();
  const homeRow = page.getByTestId("home-root");
  await expect(homeRow).toContainText("your home folder", { timeout: 30_000 });
  const home = (await homeRow.locator("code").textContent())!.trim();
  expect(home).toMatch(/^\//);

  const field = page.getByLabel("Folder for agents");
  await field.fill("~/Movies/tasklet-application");
  await expect(page.getByTestId("folder-resolved")).toHaveText(`Folder: ${home.replace(/\/+$/, "")}/Movies/tasklet-application`);
  await field.fill("~");
  await expect(page.getByTestId("folder-resolved")).toHaveText(`Folder: ${home.replace(/\/+$/, "")}`);
  await field.fill("/Volumes/Untitled");
  await expect(page.getByTestId("folder-resolved")).toHaveText("Folder: /Volumes/Untitled");

  // the dev bridge never runs an Owner command
  await field.fill("~");
  await page.getByRole("button", { name: "Add folder…" }).click();
  await expect(page.locator(".toast")).toContainText("no Owner command runs from the dev bridge");
  expect(errors).toEqual([]);
});
