// A video's frame (Home's Recent videos, the project map's videos): the film icon until
// the frame arrives, then the frame.
import { createEffect, createResource, Show } from "solid-js";
import { actions } from "../actions";
import { Icon } from "../design/Icon";

/** A recent video's frame, drawn once it arrives (the film icon until then). */
export function RecentThumb(props: { path: string; missing: boolean }) {
  let canvas: HTMLCanvasElement | undefined;
  const [thumb] = createResource(
    () => (props.missing ? undefined : props.path),
    async (p) => {
      try {
        return await actions.videoThumb(p);
      } catch {
        return null;
      }
    },
  );
  createEffect(() => {
    const t = thumb();
    if (!t || !canvas) return;
    const bin = atob(t.rgb);
    const rgba = new Uint8ClampedArray(t.w * t.h * 4);
    for (let k = 0, j = 0; k < t.w * t.h; k++, j += 3) {
      rgba[k * 4] = bin.charCodeAt(j);
      rgba[k * 4 + 1] = bin.charCodeAt(j + 1);
      rgba[k * 4 + 2] = bin.charCodeAt(j + 2);
      rgba[k * 4 + 3] = 255;
    }
    canvas.width = t.w;
    canvas.height = t.h;
    canvas.getContext("2d")?.putImageData(new ImageData(rgba, t.w, t.h), 0, 0);
  });
  return (
    <>
      <Show when={!thumb()}>
        <Icon name="film" size={22} />
      </Show>
      <canvas ref={canvas} class="recent-img" data-testid="recent-thumb" data-from={thumb()?.from ?? ""} hidden={!thumb()} aria-hidden="true" />
    </>
  );
}

