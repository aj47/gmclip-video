// U.09, AGENTS.md (h53): when the text hook outlasts the opening take and the next take
// changes the layout under it, the Short tab says where, and "Keep the opening layout
// until the hook clears" fixes it (the take is split at a word near the hook's end and
// the piece before takes the opening layout); the warning goes away.
import { expect, test, type Page } from "@playwright/test";
import { copyFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openPage, VIDEO } from "./ux-lib";
import { openTab } from "./tabs";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;
async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project = ""): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

test("the hook keeps the opening layout until it clears", async ({ browser }) => {
  test.setTimeout(240_000);
  const copy = join(mkdtempSync(join(tmpdir(), "gmclip-hl-")), "hook-layout.mp4");
  copyFileSync(VIDEO, copy);
  const { page, errors } = await openPage(browser, { width: 1100, height: 700 });
  await page.getByLabel("Video path").fill(copy);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toHaveText("hook-layout", { timeout: 30_000 });
  // the viewer has the cut loaded (its ids for the bridge calls below)
  let vs = { project: "", cut: "" };
  await expect.poll(async () => {
    vs = await bridge<{ project: string; cut: string }>(page, "viewer_state", {});
    return !!vs.cut && !!vs.project;
  }, { timeout: 20_000 }).toBe(true);
  // two takes: split at 2 s; the second take gets its own crop (another layout)
  await page.locator(".cut-head").hover();
  await page.keyboard.press("Home");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:00\.00 /);
  for (let i = 0; i < 6; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(page.getByTestId("timecode")).toHaveText(/^0:02\.00/);
  await page.keyboard.press("s");
  await expect.poll(async () => (await page.getByTestId("cut-meta").textContent()) ?? "").toContain("2 parts");
  const cv = await bridge<{ parts: { id: string; layers: { model: string | null }[] }[] }>(page, "cut_view", { cut: vs.cut }, vs.project);
  const p2 = cv.parts[1]!;
  await bridge(page, "layer_crop_norm", { cut: vs.cut, part: p2.id, layer: p2.layers[0]!.model, x: 0.1, y: 0.1, w: 0.5, h: 0.5 }, vs.project);

  // a 5 s hook: still up when take 2 starts
  await openTab(page, "Short");
  await page.getByLabel("Hook text").fill("My PC made this");
  await page.getByLabel("Hook seconds").fill("5");
  await page.getByRole("button", { name: "Set hook" }).click();
  const warn = page.getByTestId("hook-layout-warn");
  await expect(warn).toContainText("(take 2) while the hook is still on screen", { timeout: 20_000 });
  await page.getByTestId("hook-layout").click();
  await expect(page.getByText(/The opening layout stays until the hook clears/)).toBeVisible({ timeout: 20_000 });
  await expect(warn).toHaveCount(0, { timeout: 20_000 });
  await expect.poll(async () => (await page.getByTestId("cut-meta").textContent()) ?? "").toContain("3 parts");
  expect(errors).toEqual([]);
  await page.close();
});
