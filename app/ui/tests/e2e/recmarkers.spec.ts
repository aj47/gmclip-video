// Recording markers on the timeline (decision 0052): the markers a cut plays (marker_list,
// in cut ms) are ticks in the ruler, and hovering one names it and its time. The core's
// answer is replaced here with two markers, since making a recording needs real
// permissions; the read itself is covered by gmclip-core's recording_markers test.
import { expect, test } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { STATE } from "./global-setup";
import { labMedia } from "./lab";

const VIDEO = labMedia("renders/own-vm-1080.mp4");

test.skip(!!process.env.GMCLIP_S2A, "GMCLIP_S2A runs the S2a spec on its own");
test.skip(!existsSync(VIDEO), "no test video on this machine");

test("recording markers are ticks in the ruler with their time on hover", async ({ browser }) => {
  test.setTimeout(120_000);
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { port: number; token: string };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  let asked = 0;
  await page.routeWebSocket(`ws://127.0.0.1:${s.port}/`, (socket) => {
    const server = socket.connectToServer();
    socket.onMessage((message) => {
      let req: { id?: number; call?: { command: string; args?: { cut?: string } } };
      try { req = JSON.parse(String(message)); } catch { server.send(message); return; }
      if (req.call?.command === "marker_list") {
        asked++;
        const markers = [
          { n: 1, cut_ms: 2000, part: "p", source: "s", src_ms: 2000 },
          { n: 2, cut_ms: 6500, part: "p", source: "s", src_ms: 6500 },
        ];
        socket.send(JSON.stringify({ id: req.id, reply: { ok: true, output: { markers } } }));
        return;
      }
      server.send(message);
    });
  });
  await page.addInitScript((info) => {
    (globalThis as unknown as { __GMCLIP_DEV_BRIDGE__: unknown }).__GMCLIP_DEV_BRIDGE__ = info;
  }, { port: s.port, token: s.token });
  await page.goto("/");
  await page.getByLabel("Video path").fill(VIDEO);
  await page.getByLabel("Video path").press("Enter");
  await expect(page.getByTestId("cut-title")).toContainText("own-vm", { timeout: 30_000 });

  const ruler = page.getByTestId("timeline-ruler");
  await expect(ruler).toHaveAttribute("data-rec-markers", "1@2000,2@6500", { timeout: 20_000 });
  expect(asked).toBeGreaterThan(0);

  // hovering the second tick names it; away from it, no tooltip
  const canvas = page.locator(".timeline-canvas");
  // the render is 30 fps, and so is the cut opened from it
  const fps = 30;
  const frames = Number(await canvas.getAttribute("data-frames"));
  const ppf = Number(await canvas.getAttribute("data-ppf"));
  const scroll = Number(await canvas.getAttribute("data-scroll"));
  expect(frames).toBeGreaterThan(6.5 * fps);
  const box = (await ruler.boundingBox())!;
  // the timeline's x of a frame: 12 px in, then ppf a frame, less the scroll
  const x = 12 + Math.round(6.5 * fps) * ppf - scroll;
  await page.mouse.move(box.x + x, box.y + box.height / 2);
  await expect(ruler).toHaveAttribute("title", "Marker 2 · 0:06.5");
  await page.mouse.move(box.x + x + 40, box.y + box.height / 2);
  await expect(ruler).not.toHaveAttribute("title", /Marker/);

  // the tick is drawn: the ruler's pixels at the dot are the danger red, not the band
  await page.mouse.move(2, 2);
  const red = await ruler.evaluate((el: HTMLCanvasElement, px: number) => {
    const g = el.getContext("2d")!;
    const k = el.width / el.clientWidth;
    const d = g.getImageData(Math.round(px * k), Math.round((24 - 10) * k), 1, 1).data;
    return d[0]! > 150 && d[1]! < 120;
  }, x);
  expect(red).toBe(true);
  expect(errors).toEqual([]);
  await page.close();
});
