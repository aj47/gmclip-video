// The caption style gallery (inspector): every caption style drawn on the current frame;
// a tile applies its style (one undo step). "Save as my style" keeps the cut's caption look
// in the project and the profile's style library.
import { baseName } from "../platform";
import { createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { actions, type CaptionGrouping, type CaptionLook, type CaptionMatchResult, type CaptionStyleRow } from "../actions";
import { attempt, cutView, say, setVideoDropHook, viewer } from "../store";
import { Button, IconButton, Panel, tip } from "../design/components";
import "../look.css";

function Thumb(props: { row: CaptionStyleRow }) {
  let el!: HTMLCanvasElement;
  onMount(() => {
    const t = props.row.thumb;
    if (!t) return;
    const bin = atob(t.rgb);
    const px = new Uint8ClampedArray(t.w * t.h * 4);
    for (let i = 0, j = 0; i < t.w * t.h; i++, j += 3) {
      px[i * 4] = bin.charCodeAt(j);
      px[i * 4 + 1] = bin.charCodeAt(j + 1);
      px[i * 4 + 2] = bin.charCodeAt(j + 2);
      px[i * 4 + 3] = 255;
    }
    el.getContext("2d")?.putImageData(new ImageData(px, t.w, t.h), 0, 0);
  });
  return <canvas ref={el} class="look-thumb" width={props.row.thumb?.w ?? 90} height={props.row.thumb?.h ?? 160} aria-hidden="true" />;
}

const GROUPING: Record<string, string> = {
  story_v5: "Story",
  single_word: "one word",
  adaptive_words: "adaptive",
  readable_phrase: "phrases",
  phrase: "≤ 5 words",
};

/** "Match a short": pick, type or drop a video; its burned-in captions are measured and
 * kept as a project style (≈, an approximation) for the gallery. */
function MatchShort(props: { onMatched: () => void }) {
  const [path, setPath] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [res, setRes] = createSignal<CaptionMatchResult | null>(null);
  const go = async (p: string) => {
    const v = cutView();
    if (!v || !p.trim() || busy()) return;
    setBusy(true);
    const r = await attempt(() => actions.captionStyleMatch(v.project, v.cut, p.trim()));
    setBusy(false);
    if (r) {
      setRes(r);
      setPath("");
      say(`≈ ${r.name} added to the styles`);
      props.onMatched();
    }
  };
  const pick = async () => {
    const r = await attempt(actions.pickVideo);
    if (r?.picked) void go(r.picked);
  };
  // while the gallery is open, a video dropped on the window is matched, not opened
  setVideoDropHook(() => (p: string) => void go(p));
  onCleanup(() => setVideoDropHook(null));
  const swatch = (c: string | null) => (c ? c.slice(0, 7) : "transparent");
  return (
    <div class="match-short" role="group" aria-label="Match a short" data-testid="match-short" aria-busy={busy()}>
      <form class="look-inline" onSubmit={(e) => { e.preventDefault(); void go(path()); }}>
        <Button type="button" variant="ghost" size="sm" icon="sparkles" data-testid="match-pick" disabled={busy()} tooltip="Measure the captions of a short you like (or drop its video here)" onClick={() => void pick()}>
          Match a short…
        </Button>
        <input aria-label="Short to match (path)" data-testid="match-path" placeholder="/path/to/short.mp4" value={path()} onInput={(e) => setPath(e.currentTarget.value)} disabled={busy()} />
      </form>
      <Show when={busy()}><span class="muted small" data-testid="match-busy">Reading its captions…</span></Show>
      <Show when={res()}>
        {(r) => (
          <div class="match-result" data-testid="match-result" data-keyword={r().keyword ?? ""} data-outline={Math.round(r().outline)} data-size={Math.round(r().size)}>
            <span class="match-name">{r().name}</span>
            <span class="match-badge" data-testid="match-approx" {...tip(r().notes.join(" · "))}>approximation</span>
            <span class="match-facts">
              <span class="look-swatch-dot" style={{ background: swatch(r().fill) }} {...tip("Fill")} />
              <Show when={r().keyword}><span class="look-swatch-dot" style={{ background: swatch(r().keyword) }} {...tip("Keyword colour")} /></Show>
              <span>{Math.round(r().size)} px · {r().weight}</span>
              <span>{r().outline >= 1 ? `outline ${Math.round(r().outline)} px` : "no outline"}</span>
              <span>{r().words_per_cue.toFixed(1)} words a cue{r().cue_s ? ` · ${r().cue_s!.toFixed(1)} s` : ""}</span>
              <span>at {Math.round(r().y * 100)} % height</span>
              <span class="muted">{r().with_text}/{r().frames} frames with captions</span>
            </span>
          </div>
        )}
      </Show>
    </div>
  );
}

export function CaptionGallery() {
  const [open, setOpen] = createSignal(false);
  const [name, setName] = createSignal("");
  const [written, setWritten] = createSignal<{ path: string; cues: number } | null>(null);
  const key = () => {
    const v = cutView();
    return open() && v ? { p: v.project, c: v.cut, rev: v.rev, f: viewer()?.frame ?? 0 } : null;
  };
  const [gallery, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.captionGallery(k.p, k.c, k.f))) ?? null);
  const run = async (fn: () => Promise<unknown>) => {
    if (await attempt(fn)) void refetch();
  };
  // the framing's own look (fine control beside the gallery's presets)
  const [look, { refetch: refetchLook }] = createResource(
    () => { const v = cutView(); return open() && v ? { p: v.project, c: v.cut, rev: v.rev } : null; },
    async (k) => (await attempt(() => actions.overlaysView(k.p, k.c))) ?? null,
  );
  /** The look last sent, until it is read back: a second change before then builds on it
   * (built on the look as last read, it sent the first change's field back as it was). */
  let sent: CaptionLook | null = null;
  const setLook = (patch: Partial<CaptionLook>) => {
    const v = cutView();
    const f = v?.framing?.active;
    if (!v || !f) return;
    const next = { ...(sent ?? look()?.caption_look ?? { animation: "none", keyword_animation: "none" }), ...patch };
    sent = next;
    void attempt(() => actions.captionLookSet(v.project, f, next)).then(async (r) => {
      if (r) await refetchLook();
      if (sent === next) sent = null;
      if (r) void refetch();
    });
  };
  const setGrouping = (g: CaptionGrouping) => {
    const v = cutView();
    const f = v?.framing?.active;
    if (v && f) void attempt(() => actions.captionGroupingSet(v.project, f, g)).then((r) => { if (r) { void refetchLook(); void refetch(); } });
  };
  const about = (r: CaptionStyleRow) =>
    [r.label, r.font, r.grouping ? GROUPING[r.grouping] ?? r.grouping : "", r.look && r.look.animation !== "none" ? r.look.animation : "", r.look?.keyword_animation === "pop" ? "keyword pop" : "", r.why ?? ""]
      .filter(Boolean)
      .join(" · ");
  return (
    <Panel
      class="look"
      label="Caption styles"
      title="Caption style"
      actions={
        <Button variant="ghost" size="sm" icon="captions" data-testid="gallery-toggle" aria-expanded={open()} tooltip="Each style drawn on the current frame" onClick={() => setOpen(!open())}>
          {open() ? "Hide" : "Styles"}
        </Button>
      }
    >
      <Show when={open()}>
        <MatchShort onMatched={() => void refetch()} />
        <div class="look-grid" role="list" data-testid="caption-gallery" aria-busy={gallery.loading}>
          <For each={gallery()?.styles ?? []}>
            {(r) => (
              <div class={r.current ? "look-tile current" : "look-tile"} role="listitem" data-testid="style-tile" data-name={r.name} data-origin={r.origin}>
                <button type="button" class="look-pick" {...tip(about(r))} disabled={!r.thumb && !!r.why && r.origin === "builtin"} aria-pressed={r.current}
                  onClick={() => { const v = cutView(); if (v) void run(() => actions.captionStyleApply(v.project, v.cut, r.name)); }}>
                  <Show when={r.thumb} fallback={<span class="look-none">{r.why ?? "…"}</span>}>
                    <Thumb row={r} />
                  </Show>
                  <span class="look-label">{r.label}</span>
                </button>
                <Show when={r.origin === "mine"}>
                  <IconButton icon="x" size="sm" class="look-forget" label={`Remove ${r.label} from my styles`}
                    onClick={() => { const v = cutView(); if (v) void run(() => actions.captionStyleForgetMine(v.project, r.name)); }} />
                </Show>
              </div>
            )}
          </For>
        </div>
        <div class="look-form" role="group" aria-label="Caption look">
          <label for="cap-grouping">Grouping</label>
          <select id="cap-grouping" data-testid="cap-grouping" value={look()?.caption_grouping ?? "story_v5"} data-value={look()?.caption_grouping ?? "story_v5"} onChange={(e) => setGrouping(e.currentTarget.value as CaptionGrouping)}>
            <For each={Object.entries(GROUPING)}>{([k, l]) => <option value={k}>{l}</option>}</For>
          </select>
          <label for="cap-anim">Animation</label>
          <select id="cap-anim" data-testid="cap-anim" value={look()?.caption_look?.animation ?? "none"} data-value={look()?.caption_look?.animation ?? "none"} onChange={(e) => setLook({ animation: e.currentTarget.value as CaptionLook["animation"] })}>
            <option value="none">none</option>
            <option value="bounce">word bounce</option>
            <option value="pop">pop</option>
            <option value="fade">fade</option>
          </select>
          <label for="cap-kw">Keywords</label>
          <div class="look-inline">
            <select id="cap-kw" data-testid="cap-kw" value={look()?.caption_look?.keyword_animation ?? "none"} data-value={look()?.caption_look?.keyword_animation ?? "none"} onChange={(e) => setLook({ keyword_animation: e.currentTarget.value as CaptionLook["keyword_animation"] })}>
              <option value="none">colour</option>
              <option value="pop">colour + pop</option>
            </select>
            <input type="color" class="look-swatch" aria-label="Keyword colour" {...tip("Keyword colour")} data-testid="cap-kw-colour" value={(look()?.caption_look?.keyword_colour ?? "#FFD400FF").slice(0, 7).toLowerCase()}
              onChange={(e) => setLook({ keyword_colour: `${e.currentTarget.value.toUpperCase()}FF` })} />
          </div>
        </div>
        <div class="look-inline" role="group" aria-label="Captions file">
          <For each={["srt", "vtt", "ass"] as const}>
            {(f) => (
              <Button variant="ghost" size="sm" icon="upload" data-testid={`captions-${f}`} tooltip={`Write the captions as a .${f} file in the export folder`}
                onClick={() => { const v = cutView(); if (v) void attempt(() => actions.captionsFile(v.project, v.cut, f)).then((r) => r && setWritten(r)); }}>
                .{f}
              </Button>
            )}
          </For>
          <Show when={written()}>
            {(w) => <span class="look-note" data-testid="captions-written" {...tip(w().path)}>{w().cues} cues · {baseName(w().path)}</span>}
          </Show>
        </div>
        <form class="look-inline" onSubmit={(e) => { e.preventDefault(); const v = cutView(); const n = name().trim(); if (v && n) void run(() => actions.captionStyleSaveMine(v.project, v.cut, n)).then(() => setName("")); }}>
          <input aria-label="Style name" data-testid="style-name" placeholder="My style" maxLength={60} value={name()} onInput={(e) => setName(e.currentTarget.value)} />
          <Button type="submit" size="sm" icon="bookmark" data-testid="style-save" disabled={!name().trim()} tooltip="Keep this cut's caption look in my styles">Save as my style</Button>
        </form>
      </Show>
    </Panel>
  );
}
