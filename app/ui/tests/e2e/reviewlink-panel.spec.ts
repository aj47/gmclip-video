// A.03 / U.10: the review-link controls on Home. Serving, a new link (either role), the
// tailnet mode and the remote-fix allowance are the Owner's: the dev bridge refuses them
// before they run, and nothing changes. Revoking and stopping take access away and need
// no sheet. A link's "Fix with agent" on loopback is only ever queued (R-REV-4): it waits
// under Needs you with the link's words as text (R-UI-SEC-2), counted on the panel, and
// is dismissed there. No video needed. The link's QR code (src/qr.ts, no dependency):
// structure checks on the encoder's output: size per version, finder and timing patterns,
// the two format copies equal and a valid BCH (15, 5) word, the dark module. (Decoding was
// checked with CoreImage's CIDetector on versions 1, 6, 13, 19 and 33: every text came
// back exact.)
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { qrEncode } from "../../src/qr";
import { STATE } from "./global-setup";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");

test("review-link controls: settings are the Owner's, a link's fix request waits as text", async ({ browser }) => {
  test.setTimeout(120_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  const bridge = <T,>(c: string, a: Record<string, unknown> = {}) =>
    page.evaluate(([c, a]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: Record<string, unknown>) => Promise<{ ok: boolean; output: T; error?: { code: string; message: string } }> }).__gmclipBridgeCall(c as string, a as Record<string, unknown>), [c, a] as const);
  const toast = page.locator(".toast.error").last();

  // every setting is the Owner's: refused before it runs, with one answer
  for (const [c, a] of [["review_serve", { on: true }], ["review_link_new", {}], ["review_link_role", { role: "editor" }], ["owner_request", { command: "review_tailnet", args: { host: "m.ts.net", owner_login: "aj" } }]] as const) {
    const r = await bridge(c, a);
    expect(r.ok, c).toBe(false);
    expect(r.error?.code, c).toBe("Unauthorized");
    expect(r.error?.message, c).toContain("dev bridge");
  }
  // H.09: the startup probe's lines for this Mac (H.264 first; HEVC and AV1 reported)
  const mac = page.getByRole("region", { name: /^This (Mac|PC)$/ });
  await expect(mac.getByTestId("mac-cap")).toHaveCount(5);
  await expect(mac.getByTestId("mac-cap").nth(1)).toContainText("H.264:");
  await expect(mac.getByTestId("mac-cap").nth(3)).toContainText("HEVC:");
  await expect(mac.getByTestId("mac-cap").nth(4)).toContainText("AV1:");
  // the review link lives in Settings; what links ask for waits on Home (Activity › Needs you)
  const toSettings = () => page.getByRole("button", { name: "Settings" }).click();
  const toHome = () => page.getByRole("button", { name: "Home" }).click();
  await toSettings();
  // the review link panel is the developer setting's (decision 0054)
  const rl = page.getByRole("region", { name: "Review link" });
  await expect(rl).toHaveCount(0);
  await page.getByTestId("pref-developer").click();
  await rl.getByTestId("review-serve").click();
  await expect(toast).toContainText("dev bridge");
  await expect(rl.getByTestId("review-state")).toContainText("Not serving");

  // dev builds only: serving and a link without the sheets
  const link = (await bridge<{ url: string }>("dev_review_link")).output.url;
  expect(link).toMatch(/^http:\/\/127\.0\.0\.1:\d+\/#t=[0-9a-f]{64}$/);
  await page.reload();
  await toSettings();
  await expect(rl.getByTestId("review-state")).toContainText("Serving at 127.0.0.1:");
  // the machine is named as the UI names it ("this Mac" / "this PC", platform.ts)
  await expect(rl.getByTestId("review-state")).toContainText(/this (Mac|PC) only/);
  await expect(rl.getByTestId("review-link-row")).toHaveCount(1);
  // a new link is the Owner's: no URL and no QR appear
  await rl.getByTestId("review-new").click();
  await expect(toast).toContainText("dev bridge");
  await rl.getByTestId("review-new-editor").click();
  await expect(toast).toContainText("dev bridge");
  await expect(rl.getByTestId("review-url")).toHaveCount(0);
  await expect(rl.getByTestId("review-qr")).toHaveCount(0);
  await expect(rl.getByTestId("review-link-row")).toHaveCount(1);
  // the tailnet mode and remote fixes: through the Owner sheet (none here)
  await rl.locator("summary").click();
  await rl.getByLabel("Tailnet host").fill("mac.tailnet.ts.net");
  await rl.getByLabel("Your tailnet login").fill("aj@github");
  await rl.getByRole("button", { name: "Use my tailnet" }).click();
  await expect(toast).toContainText("dev bridge");
  // the machine is named as the UI names it ("this Mac" / "this PC", platform.ts)
  await expect(rl.getByTestId("review-state")).toContainText(/this (Mac|PC) only/);
  await rl.getByRole("button", { name: "Allow" }).first().click();
  await expect(toast).toContainText("dev bridge");
  await expect(rl).toContainText("Off: a fix request from a link waits for your click here.");

  const linkId = (await rl.getByTestId("review-link-row").textContent())!.slice(0, 8);
  await toHome();

  // the link asks for a fix: on loopback it is only ever queued
  const url = new URL(link);
  const linkToken = url.hash.slice(3);
  const ctx = await browser.newContext({ bypassCSP: true });
  const ext = await ctx.newPage();
  await ext.goto(`${url.origin}/`);
  const note = '<img src=x onerror="window.__xss=1">tighten the start';
  const res = await ext.evaluate(async ([token, note]) => {
    const sess = await fetch("/session", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token }) });
    const h = (await (await fetch("/api/home")).json()) as { csrf: string };
    const f = await fetch("/api/fix", { method: "POST", headers: { "Content-Type": "application/json", "X-CSRF": h.csrf }, body: JSON.stringify({ cut: "c1", note }) });
    return { session: sess.status, fix: f.status, body: (await f.json()) as { started: boolean } };
  }, [linkToken, note] as const);
  expect(res.session).toBe(200);
  expect(res.fix).toBe(200);
  expect(res.body.started).toBe(false);

  const ask = page.getByTestId("review-ask").filter({ hasText: "asks for an agent fix" });
  await expect(ask).toHaveCount(1, { timeout: 20_000 });
  await expect(ask).toHaveAttribute("data-action", "remote_fix");
  const shown = ask.getByTestId("review-ask-note");
  await expect(shown).toContainText(note);
  await expect(shown).toContainText(`review link ${linkId}`);
  await expect(ask.locator("img")).toHaveCount(0);
  expect(await page.evaluate(() => (globalThis as unknown as { __xss?: number }).__xss)).toBeUndefined();
  await toSettings();
  await expect(rl.getByTestId("review-asks")).toContainText("1 request from links waits for your click under Needs you (1 “Fix with agent”)");
  await toHome();
  await ask.getByTestId("review-ask-dismiss").click();
  await expect(page.getByTestId("review-ask").filter({ hasText: "asks for an agent fix" })).toHaveCount(0);
  await toSettings();
  await expect(rl.getByTestId("review-asks")).toHaveCount(0);

  // revoking needs no sheet, and ends the link's session at once
  await rl.getByRole("button", { name: /^Revoke link / }).click();
  await expect(rl.getByTestId("review-link-row")).toHaveCount(0);
  await expect(rl).toContainText("No live links.");
  expect(await ext.evaluate(async () => (await fetch("/api/home")).status)).toBe(401);
  await ctx.close();
  // stopping takes access away too (no sheet); the next spec starts from "Not serving"
  await rl.getByTestId("review-stop").click();
  await expect(rl.getByTestId("review-state")).toContainText("Not serving");
  expect(errors).toEqual([]);
});

const LINK = "https://mac.tailnet.ts.net:8443/#t=" + "0123456789abcdef".repeat(4);

function formatWord(m: boolean[][]): [number, number] {
  const s = m.length;
  const bit = (x: number, y: number) => (m[y]![x] ? 1 : 0);
  let a = 0;
  let b = 0;
  const first: [number, number][] = [];
  for (let i = 0; i <= 5; i++) first.push([8, i]);
  first.push([8, 7], [8, 8], [7, 8]);
  for (let i = 9; i < 15; i++) first.push([14 - i, 8]);
  first.forEach(([x, y], i) => (a |= bit(x, y) << i));
  const second: [number, number][] = [];
  for (let i = 0; i < 8; i++) second.push([s - 1 - i, 8]);
  for (let i = 8; i < 15; i++) second.push([8, s - 15 + i]);
  second.forEach(([x, y], i) => (b |= bit(x, y) << i));
  return [a, b];
}

test("the QR encoder draws valid codes", () => {
  for (const [text, ver] of [["hi", 1], [LINK, 6], ["x".repeat(300), 13]] as const) {
    const m = qrEncode(text);
    expect(m.length).toBe(ver * 4 + 17);
    const s = m.length;
    // finder patterns: dark 7×7 ring, light ring, dark 3×3 core
    for (const [cx, cy] of [[3, 3], [s - 4, 3], [3, s - 4]]) {
      expect(m[cy]![cx]).toBe(true);
      expect(m[cy - 2]![cx]).toBe(false);
      expect(m[cy - 3]![cx]).toBe(true);
    }
    for (let i = 8; i < s - 8; i++) {
      expect(m[6]![i]).toBe(i % 2 === 0);
      expect(m[i]![6]).toBe(i % 2 === 0);
    }
    expect(m[s - 8]![8]).toBe(true);
    const [a, b] = formatWord(m);
    expect(a).toBe(b);
    const w = a ^ 0x5412;
    let rem = w >>> 10;
    for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
    expect(w).toBe(((w >>> 10) << 10) | rem);
    expect((w >>> 13) & 3).toBe(0); // level M
  }
  expect(() => qrEncode("z".repeat(3000))).toThrow();
});
