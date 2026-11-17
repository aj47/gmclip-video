// Waiting for a video's analysis by its progress, not by a fixed time: the transcript and
// then the level the audio lane draws take a minute on a quiet Mac and several while the
// suite or other builds load it. Fails only when the analysis fails or stops moving.
import { expect, type Page } from "@playwright/test";

type Bridge = (command: string, args: Record<string, unknown>, project?: string) => Promise<{ ok: boolean; output?: unknown; error?: unknown }>;

interface Row {
  phase: string;
  done_s: number;
  error: string | null;
}

async function call<T>(page: Page, command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const r = await page.evaluate(
    ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: Bridge }).__gmclipBridgeCall(c, a, p),
    [command, args, project] as [string, Record<string, unknown>, string | undefined],
  );
  if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
  return r.output as T;
}

/** Waits until every source of the open project named like `name` is analysed. Fails
 * when one fails, or when nothing moves (phase or seconds heard) for `stallMs`. */
export async function analysed(page: Page, name: string, stallMs = 120_000): Promise<void> {
  let last = "";
  let movedAt = Date.now();
  for (;;) {
    const open = await call<{ id: string; name: string }[]>(page, "project_list_open", {});
    const pid = open.find((p) => p.name.includes(name))?.id;
    if (pid) {
      const st = await call<{ sources: Row[] }>(page, "analysis_status", {}, pid);
      const failed = st.sources.find((s) => s.phase === "failed");
      expect(failed?.error ?? null, "the analysis failed").toBeNull();
      if (st.sources.length && st.sources.every((s) => s.phase === "done")) return;
      const now = JSON.stringify(st.sources.map((s) => [s.phase, Math.floor(s.done_s)]));
      if (now !== last) {
        last = now;
        movedAt = Date.now();
      }
    }
    expect(Date.now() - movedAt, `the analysis of ${name} stopped moving (${last})`).toBeLessThan(stallMs);
    await page.waitForTimeout(1_000);
  }
}

/** Waits until every source of the open project named like `name` has its transcript, or
 * its analysis has ended (done or failed: what the spec makes of that is its own
 * business). The "transcribing" badge alone is no such wait: it is not up yet in the
 * moment before the analysis starts, so waiting for it to go passed at once
 * (openings.spec then asked for moments on media with no transcript yet). Fails only
 * when nothing moves for `stallMs`. */
export async function transcribed(page: Page, name: string, stallMs = 120_000): Promise<void> {
  let last = "";
  let movedAt = Date.now();
  for (;;) {
    const open = await call<{ id: string; name: string }[]>(page, "project_list_open", {});
    const pid = open.find((p) => p.name.includes(name))?.id;
    if (pid) {
      const st = await call<{ sources: (Row & { transcript: boolean })[] }>(page, "analysis_status", {}, pid);
      if (st.sources.length && st.sources.every((s) => s.transcript || s.phase === "done" || s.phase === "failed")) return;
      const now = JSON.stringify(st.sources.map((s) => [s.phase, Math.floor(s.done_s)]));
      if (now !== last) {
        last = now;
        movedAt = Date.now();
      }
    }
    expect(Date.now() - movedAt, `the transcript of ${name || "the video"} stopped moving (${last})`).toBeLessThan(stallMs);
    await page.waitForTimeout(1_000);
  }
}
