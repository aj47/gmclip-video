// Decision 0024, chapters from topics: a long-form made from three topics shows its
// YouTube chapters (00:00 first, each ≥ 10 s) in a bar with Copy, which puts the lines on
// the clipboard; one made from two topics says why YouTube will not show them. Skips
// without the video.
import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { transcribed } from "./analysed";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

async function bridge<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

test("a long-form's chapters, copied for YouTube", async ({ browser }) => {
  test.setTimeout(1_200_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.grantPermissions(["clipboard-read", "clipboard-write"]);
  const page = await ctx.newPage();
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });
  await expect(page.getByTestId("transcribing")).toHaveCount(0, { timeout: 600_000 });
  await transcribed(page, "");
  const open = await bridge<{ id: string; name: string }[]>(page, "project_list_open", {});
  const pid = open.find((p) => p.name.includes("own-vm"))!.id;
  const stream = (await bridge<{ cut: string }>(page, "stream_edit_ensure", {}, pid)).cut;
  const tv = await bridge<{ start_ms: number[] }>(page, "transcript_view", { cut: stream }, pid);
  const at = (sec: number) => tv.start_ms.findIndex((x) => x >= sec * 1000);
  // three topics of ~12 s each (the video is 55 s), labelled from the transcript's words
  const topics: string[] = [];
  for (const [a, b, title] of [[0, 12, "Why a VM"], [15, 28, "Skills per VM"], [32, 45, "Hardware"]] as const) {
    const r = await bridge<{ topic: string }>(page, "topic_label_ord", { cut: stream, from: at(a), to: at(b) - 1, title }, pid);
    topics.push(r.topic);
  }
  // a long-form from two of them first: too few for YouTube, and it says so
  const two = await bridge<{ cut: string }>(page, "longform_from_topics", { topics: topics.slice(0, 2), order: "source", title: "Two" }, pid);
  const three = await bridge<{ cut: string }>(page, "longform_from_topics", { topics, order: "source", title: "Three" }, pid);
  const map = page.getByTestId("project-map");
  await expect(map.getByTestId("map-long")).toHaveCount(2, { timeout: 20_000 });
  await map.getByTestId("map-long").filter({ hasText: "Two" }).getByRole("button").first().click();
  const bar = page.getByTestId("chapters-bar");
  await expect(bar).toHaveAttribute("data-valid", "false", { timeout: 20_000 });
  await expect(bar.getByTestId("chapters-why-not")).toContainText("at least 3");

  await map.getByTestId("map-long").filter({ hasText: "Three" }).getByRole("button").first().click();
  await expect(bar).toHaveAttribute("data-valid", "true", { timeout: 20_000 });
  await expect(bar).toContainText("Chapters 3");
  await bar.getByTestId("chapters-show").click();
  const lines = ((await bar.getByTestId("chapters-lines").textContent()) ?? "").trim().split("\n");
  expect(lines).toHaveLength(3);
  expect(lines[0]).toBe("00:00 Why a VM");
  expect(lines[1]).toMatch(/^00:1\d Skills per VM$/);
  expect(lines[2]).toMatch(/^00:2\d Hardware$/);
  // what the bar shows is what agents read
  const ch = await bridge<{ youtube: string; valid: boolean }>(page, "longform_chapters", { cut: three.cut }, pid);
  expect(ch.youtube).toBe(lines.join("\n"));
  await bar.getByTestId("chapters-copy").click();
  await expect(page.locator(".toast").filter({ hasText: "Chapters copied (3)" }).first()).toBeVisible();
  // (the Windows clipboard hands text back with CRLF line ends)
  expect((await page.evaluate(() => navigator.clipboard.readText())).replace(/\r\n/g, "\n")).toBe(ch.youtube);
  expect(two.cut).not.toBe(three.cut);
  // a short has no chapters bar
  await map.getByTestId("map-stream").click();
  await expect(bar).toHaveCount(0);
  expect(errors).toEqual([]);
});
