// The Thumbnail tab (decision 0015; V.06): scrub to a frame and "Use this frame" with a
// title (the text hook by default, the brand kit's look when the project has one); every
// version stays here and one is picked. "Make thumbnail with my agent" asks your agent for
// a cover frame and three text-hook options, which preview on the frame; pick one. Render
// writes the PNG beside the project with the export's own compositor and fonts.
import { createEffect, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { actions, type ThumbVersion } from "../actions";
import { Button, Chip, Empty, IconButton, Panel, tip } from "../design/components";
import { attempt, cutView, say, viewer } from "../store";
import { setTitlePreview } from "../collab";
import { who } from "./CommentsPanel";
import { CopyDrafts } from "./Packaging";

function b64bytes(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** A title as the thumbnail draws it: `*word*` is the coloured word, shown without its stars. */
function TitleText(props: { text: string }) {
  return (
    <For each={props.text.split(/(\*[^*]+\*)/)}>
      {(part) => (part.length > 2 && part.startsWith("*") && part.endsWith("*") ? <mark class="title-accent">{part.slice(1, -1)}</mark> : part)}
    </For>
  );
}

export function ThumbPanel() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [data, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.thumbs(k.p, k.c))) ?? null);
  const timer = setInterval(() => void refetch(), 6000);
  onCleanup(() => {
    clearInterval(timer);
    setTitlePreview(null);
  });
  const [title, setTitle] = createSignal<string | null>(null);
  const [shown, setShown] = createSignal<string | null>(null);
  const [preview, setPreview] = createSignal<{ id: string; path: string } | null>(null);
  let canvas: HTMLCanvasElement | undefined;
  const typed = () => title() ?? data()?.text_hook ?? "";
  const here = () => viewer()?.frame ?? 0;
  const v = () => cutView()!;
  // what the frame shows: the version being looked at, else the title being typed
  createEffect(() => {
    const d = data();
    if (!d) return;
    const ver = d.versions.find((x) => x.id === shown());
    if (ver) setTitlePreview({ title: ver.title, style: ver.style, box: ver.layout.title_box, align: ver.layout.align });
    else if (typed().trim()) setTitlePreview({ title: typed(), style: d.style, box: d.layout.title_box, align: d.layout.align });
    else setTitlePreview(null);
  });
  const look = (t: ThumbVersion, option?: string) => {
    setShown(t.id);
    if (t.frame !== null) void attempt(() => actions.viewerSeek(t.frame!));
    if (option !== undefined) setTitlePreview({ title: option, style: t.style, box: t.layout.title_box, align: t.layout.align });
  };
  const render = async (t: ThumbVersion) => {
    const r = await attempt(() => actions.thumbRender(v().project, t.id));
    if (!r) return;
    setPreview({ id: t.id, path: r.path });
    const bmp = await createImageBitmap(new Blob([b64bytes(r.preview_png).buffer as ArrayBuffer], { type: "image/png" }));
    if (canvas) {
      canvas.width = bmp.width;
      canvas.height = bmp.height;
      canvas.getContext("2d")?.drawImage(bmp, 0, 0);
    }
    say("Thumbnail rendered");
    void refetch();
  };
  return (
    <div class="side-pane thumb-panel" data-testid="thumb-panel">
      <Panel
        label="Thumbnail"
        title={<>Thumbnail <Show when={data()?.brand}><Chip tone="accent" tooltip="Colours and face from the project's brand kit">brand kit</Chip></Show></>}
        actions={
          <Button size="sm" icon="bot" data-testid="thumb-agent" tooltip="Your agent picks a cover frame and writes three text-hook options; you pick one" onClick={() => void attempt(() => actions.thumbAgent(v().project, v().cut, here())).then((r) => r && say("Your agent is making thumbnail options"))}>
            With my agent
          </Button>
        }
      >
        <form
          class="comment-compose"
          onSubmit={(e) => {
            e.preventDefault();
            void attempt(() => actions.thumbAdd(v().project, v().cut, here(), typed(), !(data()?.versions.length))).then((r) => {
              if (r !== undefined) {
                setTitle(null);
                setShown(null);
                void refetch();
              }
            });
          }}
        >
          <input type="text" aria-label="Thumbnail title" data-testid="thumb-title" placeholder="Title (wrap a word in *…* to colour it)" maxLength={200} value={typed()} onInput={(e) => { setShown(null); setTitle(e.currentTarget.value); }} onKeyDown={(e) => e.stopPropagation()} />
          <Button type="submit" size="sm" variant="primary" icon="image" data-testid="thumb-use-frame" tooltip="A new version: this frame with this title">
            Use this frame
          </Button>
        </form>
        <Show when={(data()?.versions ?? []).length} fallback={<Empty compact icon="image" title="No thumbnail yet" />}>
          <ul class="thumbs" aria-label="Thumbnail versions">
            <For each={data()!.versions}>
              {(t) => (
                <li class="thumb" classList={{ picked: t.picked, shown: shown() === t.id }} data-testid="thumb" data-picked={String(t.picked)}>
                  <div class="thread-head">
                    <Show when={t.frame !== null} fallback={<Chip tone="warning" tooltip="Its frame is cut out now: set another">no frame</Chip>}>
                      <button type="button" class="chip chip-accent time-chip" {...tip("Show it on the frame")} onClick={() => look(t)}>
                        frame {t.frame}
                      </button>
                    </Show>
                    <Show when={!t.human}>
                      <Chip icon="bot">{who(t.author)}</Chip>
                    </Show>
                    <Show when={t.picked}>
                      <Chip tone="success" icon="star">picked</Chip>
                    </Show>
                    <span class="thread-actions">
                      <IconButton icon="star" size="sm" label="Pick this one" data-testid="thumb-pick" disabled={t.picked} onClick={() => void attempt(() => actions.thumbSet(v().project, t.id, { pick: true })).then(() => refetch())} />
                      <IconButton icon="image" size="sm" label="Cover from the playhead" onClick={() => void attempt(() => actions.thumbCover(v().project, v().cut, t.id, here())).then(() => refetch())} />
                      <IconButton icon="upload" size="sm" label="Render the PNG" data-testid="thumb-render" disabled={t.frame === null} onClick={() => void render(t)} />
                    </span>
                  </div>
                  <p class="thread-text" classList={{ untrusted: !t.human }} onClick={() => look(t)}><TitleText text={t.title} /></p>
                  <Show when={t.hook_options.length}>
                    <div class="hook-options" role="group" aria-label="Text-hook options">
                      <For each={t.hook_options}>
                        {(o) => (
                          <button type="button" class="hook-option" classList={{ current: o === t.title }} data-testid="hook-option" aria-pressed={o === t.title} onMouseEnter={() => look(t, o)} onFocus={() => look(t, o)} onClick={() => void attempt(() => actions.thumbSet(v().project, t.id, { title: o })).then(() => refetch())}>
                            <TitleText text={o} />
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>
                  <Show when={t.notes}>
                    <p class="muted small untrusted">{t.notes}</p>
                  </Show>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <Show when={preview()}>
          {(pv) => (
            <figure class="thumb-render" data-testid="thumb-rendered">
              <canvas ref={canvas} aria-label="The rendered thumbnail" />
              <figcaption class="muted small">{pv().path}</figcaption>
            </figure>
          )}
        </Show>
      </Panel>
      <CopyDrafts />
    </div>
  );
}
