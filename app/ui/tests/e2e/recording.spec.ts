// Recording (decision 0045): Settings › Recording lists the five permissions with the
// switch off by default, and Home offers the way there instead of a Record button. The
// switch is not turned on here: that asks the OS for real permissions.
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { STATE } from "./global-setup";

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(process.platform !== "darwin", "recording is macOS only for now");

test("Settings › Recording shows the permissions; Home points there", async ({ browser }) => {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByTestId("record-open")).toHaveCount(0);
  await page.getByTestId("record-hint").click();
  const section = page.getByTestId("rec-settings");
  await expect(section).toBeVisible();
  await expect(section.getByTestId("rec-perm")).toHaveCount(5);
  await expect(section.getByRole("switch").or(section.getByRole("checkbox")).first()).not.toBeChecked();
  expect(errors).toEqual([]);
});

test("capture overload is visible and a failed stop keeps recovery across reloads", async ({ browser }) => {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  let running = true;
  let opens = 0;
  const dir = "/tmp/gmclip-recording-recovery-test";
  await page.routeWebSocket(`ws://127.0.0.1:${s.port}/`, (socket) => {
    const server = socket.connectToServer();
    socket.onMessage((message) => {
      let req: { id?: number; call?: { command: string } };
      try { req = JSON.parse(String(message)); } catch { server.send(message); return; }
      const reply = (r: unknown) => socket.send(JSON.stringify({ id: req.id, reply: r }));
      switch (req.call?.command) {
        case "recording_status":
          reply({ ok: true, output: { enabled: true, folder: "/tmp", recording: running ? {
            dir, started: true, elapsed_s: 10, markers: 0, missing: [], stop_asked: false, trouble: null, dropped: 17,
          } : null } });
          return;
        case "recording_stop":
          running = false;
          reply({ ok: false, error: { code: "Internal", message: "Test import failure" } });
          return;
        case "recording_open":
          opens++;
          reply({ ok: false, error: { code: "Internal", message: "Test import failure" } });
          return;
        default: server.send(message);
      }
    });
  });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await expect(page.getByTestId("rec-overload")).toContainText("Capture has fallen behind");
  await page.getByTestId("rec-stop").click();
  await expect(page.getByTestId("rec-recovery")).toContainText(dir);
  await expect(page.getByTestId("rec-bar")).toHaveCount(0);
  await page.reload();
  const recovery = page.getByTestId("rec-recovery");
  await expect(recovery).toContainText(dir);
  await recovery.getByRole("button", { name: "Try opening again" }).click();
  await expect.poll(() => opens).toBe(1);
  await expect(recovery).toBeVisible();
  await recovery.getByRole("button", { name: "Dismiss" }).click();
  await expect(recovery).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem("gmclip-recording-recovery"))).toBeNull();
  await page.close();
});
