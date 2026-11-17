// "Are these the same recording?": AJ's loc_4fadb756 proxy (voice) and its 1440p MKV
// (picture), RT's measured offset 54.733 s; the Inspector nudges the picture by a frame.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";
import { openTab } from "./tabs";

const PROXY = labMedia("preview/loc_4fadb756.proxy.mp4");
const MKV = labMedia("streams/jMcAmooE53I-source.mkv");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(PROXY) || !existsSync(MKV), "AJ's stream files are not on this machine");

test("open a stream with its hi-res picture", async ({ browser }) => {
  test.setTimeout(240_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByTestId("open-more").click();
  await page.getByTestId("open-pair").click();
  const d = page.getByRole("dialog", { name: "Same recording" });
  await d.getByLabel("Sound file path").fill(PROXY);
  await d.getByLabel("Hi-res file path").fill(MKV);
  // measured from the two files: RT's −503 frames, then −1642 after the hi-res file's gap
  await expect(d.getByTestId("pair-found")).toBeVisible({ timeout: 90_000 });
  await expect(d.getByLabel("Offset (s)")).toHaveValue("16.767");
  await expect(d.getByTestId("pair-found")).toContainText("503 frames");
  await expect(d.getByTestId("pair-found")).toContainText("1642 frames");
  await expect(d.getByTestId("pair-found")).toContainText("checked on the pictures");
  await expect(d.getByLabel("Offset change 1 (s)")).toHaveValue("54.733");
  await expect(d.getByTestId("pair-found")).toContainText("no picture matching");
  await d.getByTestId("pair-open").click();
  await expect(page.getByTestId("cut-title")).toHaveText("loc_4fadb756.proxy", { timeout: 60_000 });
  await openTab(page, "Inspector");
  const off = page.getByLabel("Picture offset (s)");
  const grp = page.getByRole("group", { name: "Hi-res picture offset" });
  await expect(off).toHaveValue("16.767");
  await expect(grp).toContainText("503 frames");
  await expect(grp).toContainText("stretch from 0 s of 2");
  await page.getByRole("button", { name: "Picture one frame later" }).click();
  await expect(off).toHaveValue("16.733", { timeout: 10_000 });
  await expect(grp).toContainText("502 frames");
  expect(errors).toEqual([]);
});
