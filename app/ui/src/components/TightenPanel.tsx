// Tighten, folded in the Words tab (decision 0054; backlog U.08 sweeps): find pauses of at least
// N s (keeping a little of each), standalone fillers, or retakes (an attempt said again or
// broken off and begun again: the earlier one goes, decision 0040), see them as a
// list and as red spans on the timeline, untick any to keep, and cut the rest as one
// transaction (one ⌘Z). Spans are measured on the audio (the quiet grid, per span), and
// fillers fused to the next word are skipped, never guessed at.
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { actions, rate, type SweepKind, type SweepPreview } from "../actions";
import { attempt, cutView, say, setSweepRibbons } from "../store";

function tc(frame: number, fps: number): string {
  const s = frame / fps;
  return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;
}

export function TightenPanel() {
  const [kind, setKind] = createSignal<SweepKind>("pauses");
  const [minS, setMinS] = createSignal("0.32");
  const [keepS, setKeepS] = createSignal("0.10");
  const [preview, setPreview] = createSignal<SweepPreview | null>(null);
  const [off, setOff] = createSignal<Set<number>>(new Set());
  const [busy, setBusy] = createSignal(false);
  const fps = () => rate(cutView()?.fps);
  const chosen = createMemo(() => (preview()?.cuts ?? []).filter((c) => !off().has(c.i)));
  const seconds = () => chosen().reduce((a, c) => a + c.seconds, 0);
  const ribbons = () => setSweepRibbons(chosen().filter((c) => c.out).map((c) => c.out!));
  onCleanup(() => setSweepRibbons([]));

  const find = async () => {
    const v = cutView();
    if (!v) return;
    setBusy(true);
    const r = await attempt(() => actions.sweepPreview(v.project, v.cut, kind(), Number(minS()), Number(keepS())));
    setBusy(false);
    setOff(new Set<number>());
    setPreview(r ?? null);
    ribbons();
    if (r && !r.cuts.length) say(kind() === "pauses" ? "No pauses that long in this cut" : kind() === "fillers" ? "No standalone fillers in this cut" : "No retakes left in this cut");
  };
  const apply = async () => {
    const v = cutView();
    const p = preview();
    if (!v || !p) return;
    setBusy(true);
    const r = await attempt(() =>
      actions.sweepApply(v.project, v.cut, p.preview_id, chosen().map((c) => c.i)),
    );
    setBusy(false);
    if (r) {
      say(`Cut ${r.applied} ${p.kind === "pauses" ? "pause" : p.kind === "fillers" ? "filler" : "retake"}${r.applied === 1 ? "" : "s"} (${seconds().toFixed(1)} s) — ⌘Z puts them back`);
      setPreview(null);
      setSweepRibbons([]);
    }
  };
  const toggle = (i: number) => {
    const s = new Set(off());
    if (s.has(i)) s.delete(i);
    else s.add(i);
    setOff(s);
    ribbons();
  };

  return (
    <section class="tighten-panel" aria-label="Tighten">
      <div class="row">
        <select aria-label="Sweep kind" value={kind()} onChange={(e) => { setKind(e.currentTarget.value as SweepKind); setPreview(null); setSweepRibbons([]); }}>
          <option value="pauses">Pauses</option>
          <option value="fillers">Fillers (um, uh)</option>
          <option value="retakes">Retakes (said again)</option>
        </select>
        <Show when={kind() === "pauses"}>
          <label>≥ <input class="num" type="number" aria-label="Pause length" min="0.15" max="5" step="0.05" value={minS()} onInput={(e) => setMinS(e.currentTarget.value)} /> s</label>
          <label>keep <input class="num" type="number" aria-label="Keep of each pause" min="0" max="1" step="0.05" value={keepS()} onInput={(e) => setKeepS(e.currentTarget.value)} /> s</label>
        </Show>
        <button type="button" data-testid="sweep-find" disabled={busy()} onClick={() => void find()}>{busy() ? "Listening…" : "Find"}</button>
      </div>
      <Show when={preview()}>
        {(p) => (
          <>
            <p class="muted small" data-testid="sweep-summary">
              {p().cuts.length} found · {chosen().length} chosen · removes {seconds().toFixed(2)} s{p().skipped ? ` · ${p().skipped} skipped (fused to a word)` : ""}
            </p>
            <div class="sweep-list" role="list" aria-label="Sweep candidates">
              <For each={p().cuts}>
                {(c) => (
                  <div class="row sweep-row" role="listitem">
                    <input type="checkbox" checked={!off().has(c.i)} aria-label={`Cut ${c.out ? tc(c.out[0], fps()) : "?"}`} onChange={() => toggle(c.i)} />
                    <span class="tc">{c.out ? tc(c.out[0], fps()) : "—"}</span>
                    <span>{c.seconds.toFixed(2)} s</span>
                    <span class="muted small reason">{c.reason}</span>
                    <button type="button" class="ghost" aria-label="Play from just before it" disabled={!c.out} onClick={() => c.out && void attempt(() => actions.viewerPlayRange(Math.max(0, c.out![0] - fps()) * (48000 / fps()), (c.out![1] + fps()) * (48000 / fps())))}>▶</button>
                  </div>
                )}
              </For>
            </div>
            <div class="row">
              <button type="button" class="primary" data-testid="sweep-apply" disabled={busy() || !chosen().length} onClick={() => void apply()}>
                Cut {chosen().length} ({seconds().toFixed(1)} s)
              </button>
              <button type="button" class="ghost" onClick={() => { setPreview(null); setSweepRibbons([]); }}>Clear</button>
            </div>
          </>
        )}
      </Show>
      <p class="muted small">The timeline shows what would go in red. Holds and crops stay on the parts that remain.</p>
    </section>
  );
}
