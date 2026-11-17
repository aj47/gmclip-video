// AJ 2026-10-15: opening a 29.97 fps video failed with "the cut does not plan yet: …
// unsupported at /sources/0/expect/video/fps". A video's cut now takes its exact rate
// (29.97 → 30000/1001, decision 0020; a picture at another rate converts, 0017): the
// video opens at its own rate, no cut error shows, and the 29.97 one exports. The tiny
// fixtures are made here with ffmpeg (a dev tool).
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { STATE } from "./global-setup";

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

const FF = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].find((p) => existsSync(p));

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!FF, "no ffmpeg on this machine");

test("29.97 and 59.94 fps videos open and plan", async ({ browser }) => {
  test.setTimeout(120_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  // one page: the frame socket takes one connection per launch (R-FRAME-1)
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  for (const [name, rate, num] of [["ntsc-2997", "30000/1001", 30000], ["ntsc-5994", "60000/1001", 60000]] as const) {
    const file = join(mkdtempSync(join(tmpdir(), "gmclip-fps-")), `${name}.mp4`);
    execFileSync(FF!, ["-v", "error", "-y", "-f", "lavfi", "-i", `testsrc2=size=640x360:rate=${rate}:duration=4`, "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=4", "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", "-shortest", file]);
    if (await page.getByRole("button", { name: "Home" }).isVisible()) await page.getByRole("button", { name: "Home" }).click();
    await page.getByLabel("Video path").fill(file);
    await page.getByLabel("Video path").press("Enter");
    const sheet = page.getByRole("dialog", { name: "Import" });
    if (await sheet.isVisible({ timeout: 5_000 }).catch(() => false)) await sheet.getByTestId("import-open").click();
    await expect(page.getByTestId("cut-title")).toHaveText(name, { timeout: 30_000 });
    await expect.poll(async () => Number((await page.getByTestId("cut-meta").getAttribute("data-seconds")) ?? 0)).toBeGreaterThan(3.5);
    await expect(page.getByTestId("cut-error")).toHaveCount(0);
    await expect(page.getByText(/expect\/video\/fps|fails validation/)).toHaveCount(0);
    // the cut runs at the video's exact rate
    const list = await bridge<{ id: string; name: string; cuts: { id: string; title: string }[] }[]>(page, "project_list_open", {}, "");
    const p = list.find((x) => x.name === name)!;
    const view = await bridge<{ fps: { num: number; den: number } }>(page, "cut_view", { cut: p.cuts.find((c) => c.title === name)!.id }, p.id);
    expect(view.fps).toEqual({ num, den: 1001 });
    if (num === 30000) {
      await page.getByTestId("export").click();
      await expect(page.locator(".export-state")).toContainText("Exported", { timeout: 120_000 });
      await expect(page.getByTestId("export-failed")).toHaveCount(0);
    }
  }
  expect(errors).toEqual([]);
});
