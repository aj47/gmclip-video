// QA prep (milestones/m2/qa-e2e.md): opens the two real streams in the QA home so their
// transcripts exist, then walks undo back to the opened state (undo across every edit the
// workflows made: trims, strikes, framing, layout, captions, keywords) and checks the cut
// is the whole stream again. The home is then copied as the workflows' base.
import { expect, test } from "@playwright/test";
import { bridge, has, note, open, openVideo, projectNamed, QA, STREAM_A, STREAM_B, timed } from "./qa-lib";

test.skip(!QA || !has(STREAM_A, STREAM_B), "GMCLIP_QA=1 and the lab media's previews");

interface CutV { frames: number; fps: { num: number }; parts: unknown[]; captions: { role: string }[]; framing?: { aspect: string } }

test("prep: both streams transcribed, undone to the whole stream", async ({ browser }) => {
  test.setTimeout(2_400_000);
  // one page for the launch (the frame socket takes one connection)
  const { page, errors } = await open(browser);
  for (const [path, name, secs] of [[STREAM_A, "loc_4fadb756", 3095], [STREAM_B, "loc_086da141", 3210]] as const) {
    await page.getByRole("button", { name: "Home" }).click();
    await openVideo(page, path, name);
    const p = (await projectNamed(page, name))!;
    const cut = p.cuts[0]!.id;
    const view = () => bridge<CutV>(page, "cut_view", { cut }, p.id);
    let n = 0;
    await timed(`${name}: undo to the start`, async () => {
      for (; n < 500; n++) {
        const v = await view();
        if (Math.abs(v.frames / v.fps.num - secs) < 1 && v.parts.length === 1 && !v.captions.length && v.framing?.aspect !== "9:16") break;
        try {
          await bridge(page, "undo", {}, p.id);
        } catch (e) {
          note(`${name}: undo refused after ${n}: ${String(e)}`);
          break;
        }
      }
    });
    note(`${name}: ${n} undos`);
    const v = await view();
    expect(Math.abs(v.frames / v.fps.num - secs)).toBeLessThan(1);
    expect(v.parts.length).toBe(1);
    expect(v.captions.filter((c) => c.role === "caption")).toHaveLength(0);
  }
  expect(errors).toEqual([]);
});
