// The import sheet (SPEC §5.1, backlog U.15): the capability line for the video and each
// audio track, with the exact fix for anything this Mac cannot do, and for a file with
// more than one audio track, "Which track is your mic?" with a 3 s preview per track
// (the same moment on each) and the suggestion by speech ratio. Then the ETA chain
// (SPEC §12.5): the transcript at this Mac's speed, and the moments when you ask (D17).
import { createSignal, For, onCleanup, Show } from "solid-js";
import type { ImportInspect } from "../actions";
import { thisMachine } from "../platform";
import { about } from "../eta";

let ctx: AudioContext | null = null;
let playing: AudioBufferSourceNode | null = null;

function play(b64: string, rate: number) {
  const bin = atob(b64);
  const n = bin.length >> 1;
  ctx ??= new AudioContext();
  const buf = ctx.createBuffer(1, Math.max(1, n), rate);
  const ch = buf.getChannelData(0);
  for (let i = 0; i < n; i++) {
    const v = bin.charCodeAt(2 * i) | (bin.charCodeAt(2 * i + 1) << 8);
    ch[i] = (v >= 0x8000 ? v - 0x10000 : v) / 32768;
  }
  playing?.stop();
  playing = ctx.createBufferSource();
  playing.buffer = buf;
  playing.connect(ctx.destination);
  playing.start();
}

const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

export function ImportSheet(props: { sheet: ImportInspect; onOpen: (voice: number | null) => void; onCancel: () => void }) {
  const s = () => props.sheet;
  const [voice, setVoice] = createSignal<number | null>(s().suggested ?? s().tracks[0]?.index ?? null);
  onCleanup(() => playing?.stop());
  const blocked = () => s().video.level === "no";
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onCancel(); }}>
      <section class="keys-dialog import-sheet" role="dialog" aria-modal="true" aria-label="Import">
        <header>
          <h2>{s().name}</h2>
          <button type="button" class="ghost" onClick={() => props.onCancel()} aria-label="Close">✕</button>
        </header>
        <p class="muted small">{s().path} · {mmss(s().duration_s)}</p>
        <div class="cap-line" data-level={s().video.level} data-testid="cap-video">
          <span class="cap-dot" aria-hidden="true" />
          <span>{s().video.text}</span>
          <Show when={s().video.fix}><p class="fix">{s().video.fix}</p></Show>
        </div>
        <Show when={s().ask}>
          <h3>Which track is your mic?</h3>
          <p class="muted small">Transcripts, moment-finding and exports use this track. Each preview plays the same 3 s (at {mmss(s().preview_at_s)}).</p>
        </Show>
        <div role={s().ask ? "radiogroup" : undefined} aria-label={s().ask ? "Which track is your mic?" : undefined}>
          <For each={s().tracks}>
            {(t) => (
              <div class="track-row" data-testid={`track-${t.n}`}>
                <Show when={s().ask}>
                  <label class="check">
                    <input type="radio" name="mic" checked={voice() === t.index} onChange={() => setVoice(t.index)} />
                    Track {t.n}
                    <Show when={s().suggested === t.index}> <span class="badge">suggested: most speech</span></Show>
                  </label>
                </Show>
                <Show when={!s().ask}><b>Audio</b></Show>
                <Show when={t.preview}>
                  <button type="button" class="ghost" aria-label={`Play 3 s of track ${t.n}`} onClick={() => play(t.preview!, s().preview_rate)}>▶ 3 s</button>
                </Show>
                <Show when={t.speech != null}>
                  <span class="muted small">speech {Math.round((t.speech ?? 0) * 100)}% · {t.level_db} dB</span>
                </Show>
                <div class="cap-line" data-level={t.line.level}>
                  <span class="cap-dot" aria-hidden="true" />
                  <span>{t.line.text}</span>
                  <Show when={t.line.fix}><p class="fix">{t.line.fix}</p></Show>
                </div>
              </div>
            )}
          </For>
        </div>
        <For each={s().notes}>{(n) => <p class="note">{n}</p>}</For>
        <Show when={s().eta}>
          {(e) => (
            <div class="eta-chain" data-testid="eta-chain">
              <p>
                <b>Transcript in {about(e().transcript_s)}</b>
                <span class="muted small"> ({e().basis === "measured" ? `${thisMachine} heard the last one at ${e().speed}× real time` : `at about ${e().speed}× real time`})</span>
                {" · then your agent finds moments when you ask it (it never starts by itself)."}
              </p>
              <p class="muted small">Meanwhile: words arrive as they are heard, so you can trim by them, and pick the vertical layout in the Look tab.</p>
            </div>
          )}
        </Show>
        <div class="row">
          <button type="button" class="primary" data-testid="import-open" onClick={() => props.onOpen(s().ask ? voice() : null)}>
            {blocked() ? "Open anyway (no preview or export)" : "Open"}
          </button>
          <button type="button" class="ghost" onClick={() => props.onCancel()}>Cancel</button>
        </div>
      </section>
    </div>
  );
}
