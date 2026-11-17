// S2a through the dev bridge (GMCLIP_S2A=1): the frame stream at 1440×900 in Chromium,
// the same socket, records and WebGL2 path the app's WKWebView uses. The WKWebView run
// is `gmclip-app --open <home> --s2a` (needs an unlocked screen: rAF stops otherwise).
import { expect, test } from "@playwright/test";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { STATE } from "./global-setup";
import { importPlanProject, labPlan } from "./lab";

test.skip(!process.env.GMCLIP_S2A, "set GMCLIP_S2A=1 to measure");

test("S2a: input-to-present and fps at 1080×1920", async ({ browser }) => {
  test.setTimeout(300_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await importPlanProject(page, "S2a", process.env.GMCLIP_S2A_PLAN ?? labPlan(resolve("../../schemas/examples/h79-v4.renderplan.json")));
  await expect(page.getByTestId("viewer-stats")).toContainText("×", { timeout: 60_000 });
  const report = await page.evaluate(() => (globalThis as unknown as { __gmclipS2a: () => Promise<unknown> }).__gmclipS2a());
  console.log(JSON.stringify(report, null, 2));
  if (process.env.GMCLIP_S2A_OUT) writeFileSync(process.env.GMCLIP_S2A_OUT, JSON.stringify(report, null, 2));
  expect(report).toBeTruthy();
});
