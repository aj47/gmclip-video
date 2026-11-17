// E.01 (SPEC §10.7.1, R-AGT-5): a 3-cut batch through the launcher with the scripted
// stand-in clients (GMCLIP_E2E_FAKE_CLIENTS=1, fixtures/fake-client.py; no model). Each
// cut's run gets its own `launched:<run>` credential: each cut carries a note naming
// another cut of the batch, its run tries to read and edit that cut, and gmclip refuses
// both (`Unauthorized{cut_binding}`); its tools cover read, edit, render and ai, never
// read_media. Each run edits its own cut, so the three runs are three credentials.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("h79/h79-v4.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!process.env.GMCLIP_E2E_FAKE_CLIENTS, "GMCLIP_E2E_FAKE_CLIENTS=1 (the scripted clients)");
test.skip(!existsSync(VIDEO), "no test video on this machine");

type Reply = { ok: boolean; output?: unknown; error?: unknown };
interface Run { id: string; cut: string; pass: string; state: string; summary: string | null; error: string | null }

test("a batch's runs are bound to their own cuts", async ({ browser }) => {
  test.setTimeout(300_000);
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
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 30_000 });
  const call = async <T,>(command: string, args: Record<string, unknown>, project?: string): Promise<T> => {
    const r = await page.evaluate(
      ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: unknown, p?: string) => Promise<Reply> }).__gmclipBridgeCall(c as string, a, p as string | undefined),
      [command, args, project] as const,
    );
    if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
    return r.output as T;
  };
  const open = await call<{ id: string; name: string; cuts: { id: string }[] }[]>("project_list_open", {});
  const p = open.find((x) => x.cuts.length > 0 && x.name.startsWith("h79-v4"))!;
  const cuts = [p.cuts[0]!.id];
  for (const t of ["second", "third"]) {
    const made = await call<{ cut?: string; output?: { cut: string } }>("cut_create", { title: t }, p.id);
    cuts.push(made.cut ?? made.output!.cut);
  }
  // each cut's note names the next cut of the batch
  for (let i = 0; i < 3; i++) await call("cut_set", { cut: cuts[i], notes: `probe-cut:${cuts[(i + 1) % 3]}` }, p.id);
  const before = new Set((await call<{ runs: Run[] }>("agent_status", {}, p.id)).runs.map((r) => r.id));
  await call("dev_batch_start", { cuts, pass: "edit_pass", harness: "claude-code", budget_usd: 0, wall_clock_h: 4, per_cut_min: 90 }, p.id);
  type B = { id: string; state: string; cuts: { cut: string; state: string; run: string | null; reason: string | null }[] };
  let b: B | undefined;
  const t0 = Date.now();
  while (Date.now() - t0 < 240_000) {
    b = (await call<{ batches: B[] }>("batch_list", {}, p.id)).batches.at(-1);
    if (b?.state === "done") break;
    await page.waitForTimeout(500);
  }
  expect(b?.state, JSON.stringify(b)).toBe("done");
  expect(b!.cuts.map((c) => c.state)).toEqual(["done", "done", "done"]);
  const runs = (await call<{ runs: Run[] }>("agent_status", {}, p.id)).runs.filter((r) => !before.has(r.id));
  expect(runs).toHaveLength(3);
  // one run per cut, each its own credential (launched:<run>), each run on its own cut
  expect(new Set(runs.map((r) => r.id)).size).toBe(3);
  expect(new Set(runs.map((r) => r.cut))).toEqual(new Set(cuts));
  for (const r of runs) {
    expect(r.state, JSON.stringify(r)).toBe("done");
    expect(r.summary, JSON.stringify(r)).toContain("retitled the cut");
    expect(r.summary, JSON.stringify(r)).toContain("another cut: read refused (cut_binding), edit refused (cut_binding)");
    expect(r.summary, JSON.stringify(r)).toContain("scopes read edit render ai");
    expect(r.summary).not.toContain("read_media");
  }
  // nobody took the other cuts over
  const titles = await call<{ id: string; name: string; cuts: { id: string; title: string }[] }[]>("project_list_open", {});
  const mine = titles.find((x) => x.id === p.id)!;
  expect(mine.cuts.map((c) => c.title)).not.toContain("taken over");
  expect(errors).toEqual([]);
});
