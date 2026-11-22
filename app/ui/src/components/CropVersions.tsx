// A cut's crop versions (framings of one shape: "screen focus" and "face big" on one short,
// sharing words and captions), next to the 16:9 / 9:16 switch in the cut's top bar. They
// left the project map on 2026-11-22 (AJ): the switch says the shape, this menu picks a
// version of it and adds one. The bar clips, so the menu hangs from the window.
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { actions } from "../actions";
import { Icon } from "../design/Icon";
import { attempt, cutView, refreshCut, say } from "../store";

export function CropVersions() {
  const [open, setOpen] = createSignal<{ x: number; y: number } | null>(null);
  const [name, setName] = createSignal("");
  const list = () => cutView()?.framing?.list ?? [];
  const active = () => cutView()?.framing?.active ?? null;
  const away = (e: Event) => { if (!(e.target as HTMLElement).closest(".crop-versions-pop, [data-testid=framing-new]")) setOpen(null); };
  const esc = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(null); };
  createEffect(() => {
    if (!open()) return;
    addEventListener("pointerdown", away, true);
    addEventListener("keydown", esc);
    onCleanup(() => { removeEventListener("pointerdown", away, true); removeEventListener("keydown", esc); });
  });
  const add = async () => {
    const v = cutView();
    const n = name().trim();
    if (!v || !n) return;
    const portrait = list().find((f) => f.h > f.w);
    const aspect = v.kind === "short" || portrait ? "9:16" : "16:9";
    const r = await attempt(() => actions.framingAdd(v.project, v.cut, n, aspect));
    if (r) {
      setName("");
      await attempt(() => actions.framingPick(v.project, v.cut, r.framing));
      await refreshCut();
      say(`Crop version “${n}” added — its own crops and layout, the same words and captions`);
    }
  };
  return (
    <>
      <button
        type="button"
        class="ghost small ibtn ibtn-sm crop-versions-btn"
        data-testid="framing-new"
        aria-label="Crop versions"
        aria-haspopup="dialog"
        aria-expanded={!!open()}
        data-tip={list().length > 1 ? `Crop versions (${list().length})` : "Crop versions: another crop of this cut, the same words and captions"}
        onClick={(e) => { const b = e.currentTarget.getBoundingClientRect(); setOpen(open() ? null : { x: b.left + b.width / 2, y: b.bottom + 6 }); }}
      >
        <Icon name="chevron-down" size={13} />
      </button>
      <Show when={open()}>
        {(at) => (
          <div class="crop-versions-pop" role="dialog" aria-label="Crop versions" data-testid="map-framings" style={{ left: `${Math.max(8, Math.min(at().x - 140, innerWidth - 288))}px`, top: `${at().y}px` }}>
            <h3>Crop versions</h3>
            <ul class="map-list">
              <For each={list()}>
                {(f) => (
                  <li classList={{ "map-item": true, on: f.id === active() }}>
                    <button
                      type="button"
                      class="map-open"
                      aria-pressed={f.id === active()}
                      onClick={() => { const v = cutView(); if (v) void attempt(() => actions.framingPick(v.project, v.cut, f.id)).then(() => refreshCut()); }}
                    >
                      <span class="map-title">{f.name}</span>
                      <span class="muted small">{f.w}×{f.h}</span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
            <form class="row" onSubmit={(e) => { e.preventDefault(); void add(); }}>
              <input type="text" class="map-input" aria-label="Framing name" placeholder="New crop version, e.g. face big" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
              <button type="submit" class="small" data-testid="framing-add" disabled={!name().trim()}>Add</button>
            </form>
          </div>
        )}
      </Show>
    </>
  );
}
