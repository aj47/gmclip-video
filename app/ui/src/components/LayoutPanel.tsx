// The layout half of the Look tab (decision 0054): output shape (9:16 / 16:9), the current part's layout preset, "apply to
// all parts", and the crop editor switch (crops are edited on the full source picture).
import { thisMachine } from "../platform";
import { createResource, createSignal, For, Show } from "solid-js";
import { actions, type PartView } from "../actions";
import { attempt, cutView, say } from "../store";
import { projectMapData } from "../streamState";
import { setDrawLayout } from "../ideasState";

export function LayoutPanel(props: { part: PartView | undefined; cropping: boolean; setCropping: (on: boolean) => void }) {
  const f = () => cutView()?.framing ?? null;
  const vertical = () => f()?.aspect === "9:16";
  const layoutName = () => {
    const p = props.part;
    return p ? (f()?.part_layouts[p.id] ?? null) : null;
  };
  const preset = async (id: string, all: boolean) => {
    const v = cutView();
    const p = props.part;
    if (!v || (!p && !all)) return;
    const r = await attempt(() => actions.layoutPreset(v.project, v.cut, all ? [] : [p!.id], id));
    if (r) say(all ? "Layout applied to every part" : `Layout applied to part ${p!.n}`);
  };
  const toAll = async () => {
    const v = cutView();
    const p = props.part;
    if (!v || !p) return;
    const r = await attempt(() => actions.layoutApplyAll(v.project, v.cut, p.id));
    if (r) say(`Part ${p.n}'s layout and crops are on every part — ⌘Z undoes it`);
  };
  // Copy this take's layout on; presets saved per profile.
  const [count, setCount] = createSignal("1");
  const copyNext = async () => {
    const v = cutView();
    const p = props.part;
    const n = Math.max(1, Math.floor(Number(count()) || 1));
    if (!v || !p) return;
    const r = await attempt(() => actions.layoutCopyNext(v.project, v.cut, p.id, n));
    if (r) say(`Part ${p.n}'s layout is on the next ${r.parts} part${r.parts === 1 ? "" : "s"} — ⌘Z undoes it`);
  };
  const inset = async () => {
    const v = cutView();
    const p = props.part;
    if (!v || !p) return;
    const r = await attempt(() => actions.insetAdd(v.project, v.cut, p.id));
    if (r) say(`Detail inset added to part ${p.n}: type its crop and place in the inspector`);
  };
  const [presets, { refetch }] = createResource(
    () => { const v = cutView(); return v && f() ? { p: v.project, c: v.cut, a: f()!.aspect } : undefined; },
    async (k) => (await attempt(() => actions.layoutPresets(k.p, k.c))) ?? null,
  );
  const [chosen, setChosen] = createSignal("");
  const [saveName, setSaveName] = createSignal("");
  const pick = () => chosen() || presets()?.presets[0]?.name || "";
  const applyPreset = async () => {
    const v = cutView();
    const p = props.part;
    if (!v || !p || !pick()) return;
    const r = await attempt(() => actions.layoutPresetApply(v.project, v.cut, pick(), [p.id]));
    if (r) say(`${pick()} on part ${p.n}`);
  };
  const savePreset = async () => {
    const v = cutView();
    const p = props.part;
    const n = saveName().trim();
    if (!v || !p || !n) return;
    const r = await attempt(() => actions.layoutPresetSave(v.project, v.cut, p.id, n));
    if (r) { say(`Saved part ${p.n}'s layout as ${n}`); setSaveName(""); setChosen(n); void refetch(); }
  };
  const deletePreset = async () => {
    const v = cutView();
    if (!v || !pick()) return;
    const r = await attempt(() => actions.layoutPresetDelete(v.project, v.cut, pick()));
    if (r) { say(`Deleted ${pick()}`); setChosen(""); void refetch(); }
  };
  const chosenRow = () => presets()?.presets.find((x) => x.name === pick());
  // a saved preset: rename it, or replace it with this part's layout
  const [renameTo, setRenameTo] = createSignal("");
  const renamePreset = async () => {
    const v = cutView();
    const to = renameTo().trim();
    if (!v || !pick() || !to || to === pick()) return;
    const r = await attempt(() => actions.layoutPresetRename(v.project, pick(), to));
    if (r) { say(`Renamed ${pick()} to ${to}`); setChosen(to); setRenameTo(""); void refetch(); }
  };
  const updatePreset = async () => {
    const v = cutView();
    const p = props.part;
    if (!v || !p || !pick()) return;
    const r = await attempt(() => actions.layoutPresetSave(v.project, v.cut, p.id, pick()));
    if (r) say(`${pick()} is now part ${p.n}'s layout`);
    void refetch();
  };
  // "Draw your layout once" (SPEC §5.7.2): a landscape video this cut plays with no layout
  const unlaid = () => {
    const v = cutView();
    if (!v) return undefined;
    return (projectMapData()?.sources ?? []).find(
      (x) => !x.layout && !x.offline && (x.width ?? 0) > (x.height ?? 0) && (x.used_by.includes(v.cut) || x.stream === v.cut),
    );
  };

  return (
    <section class="layout-panel" aria-label="Layout">
      <Show when={f()} fallback={<p class="muted">This cut has no output shape yet: pick 16:9 or 9:16 at the top of the cut.</p>}>
        <Show when={unlaid()}>
          {(x) => (
            <div class="layout-callout" data-testid="layout-callout">
              <span class="small">“{x().name}” has no layout yet, so its cuts have no face. Box its screen, camera, chat and private parts once.</span>
              <button type="button" class="primary small" data-testid="layout-callout-draw" onClick={() => { const v = cutView(); if (v) setDrawLayout({ project: v.project, source: x().id }); }}>
                Draw layout…
              </button>
            </div>
          )}
        </Show>
        <div>
          <h3>Part {props.part?.n ?? "—"}</h3>
          <p class="muted small" data-testid="part-layout">{layoutName() ?? "Default: centred crop"}</p>
        </div>
        <div>
          <h3>Layout</h3>
          <div class="row">
            <For each={f()?.presets ?? []}>
              {(pr) => (
                <button type="button" disabled={!props.part} onClick={() => void preset(pr.id, false)} title={`Use ${pr.name} for this part`}>
                  {pr.name}
                </button>
              )}
            </For>
          </div>
          <div class="row">
            <button type="button" disabled={!props.part} onClick={() => void toAll()} data-testid="apply-all">Apply this part's layout and crops to all parts</button>
            <button type="button" disabled={!props.part} data-testid="apply-same-source" title="Every part cut from the same video as this one" onClick={() => { const v = cutView(); const p = props.part; if (v && p) void attempt(() => actions.layoutCopySameSource(v.project, v.cut, p.id)).then((r) => r && say(`Part ${p.n}'s layout and crops are on ${r.parts} more part${r.parts === 1 ? "" : "s"} from the same video — ⌘Z undoes it`)); }}>
              … to all parts from this video
            </button>
          </div>
          <form class="row" onSubmit={(e) => { e.preventDefault(); void copyNext(); }}>
            <button type="submit" disabled={!props.part} data-testid="copy-next">Copy this part's layout to the next</button>
            <input class="num" type="number" min="1" step="1" aria-label="How many parts" value={count()} onInput={(e) => setCount(e.currentTarget.value)} />
            <span class="muted small">parts</span>
          </form>
          <div class="row">
            <button type="button" disabled={!props.part} onClick={() => void inset()} data-testid="inset-add" title="A piece of the screen as its own box on this part (Run button, progress, taskbar clock)">Add a detail inset</button>
          </div>
        </div>
        <div>
          <h3>Presets</h3>
          <div class="row">
            <select aria-label="Layout preset" data-testid="preset-pick" value={pick()} onChange={(e) => setChosen(e.currentTarget.value)} disabled={!presets()?.presets.length}>
              <For each={presets()?.presets ?? []} fallback={<option value="">No presets yet</option>}>
                {(x) => <option value={x.name}>{x.name}{x.insets ? ` (+${x.insets} inset${x.insets === 1 ? "" : "s"})` : ""}</option>}
              </For>
            </select>
            <button type="button" disabled={!props.part || !pick()} onClick={() => void applyPreset()} data-testid="preset-apply">Use on this part</button>
            <Show when={chosenRow()?.saved}>
              <button type="button" class="ghost" aria-label={`Delete the preset ${pick()}`} onClick={() => void deletePreset()}>✕</button>
            </Show>
          </div>
          <Show when={chosenRow()?.saved}>
            <form class="row" data-testid="preset-edit" onSubmit={(e) => { e.preventDefault(); void renamePreset(); }}>
              <input type="text" aria-label="New name for the preset" data-testid="preset-rename" maxLength={60} placeholder={`Rename ${pick()}`} value={renameTo()} onInput={(e) => setRenameTo(e.currentTarget.value)} />
              <button type="submit" disabled={!renameTo().trim() || renameTo().trim() === pick()} data-testid="preset-rename-go">Rename</button>
              <button type="button" disabled={!props.part} onClick={() => void updatePreset()} data-testid="preset-update" title="Replace this preset with this part's layout">Update from part {props.part?.n ?? ""}</button>
            </form>
          </Show>
          <form class="row" onSubmit={(e) => { e.preventDefault(); void savePreset(); }}>
            <input type="text" aria-label="Preset name" data-testid="preset-name" placeholder="name (e.g. prompt + Run button)" value={saveName()} onInput={(e) => setSaveName(e.currentTarget.value)} />
            <button type="submit" disabled={!props.part || !saveName().trim()} data-testid="preset-save">Save this part's layout</button>
          </form>
          <p class="muted small">{presets()?.profile ? "Kept with your profile" : `Kept on ${thisMachine} (no profile chosen)`}; the profile's stream layouts show here too.</p>
        </div>
        <Show when={f()}>
        <div>
          <h3>Crops</h3>
          <div class="row">
            <button type="button" aria-pressed={props.cropping} classList={{ primary: props.cropping }} onClick={() => props.setCropping(!props.cropping)} data-testid="crop-mode">
              {props.cropping ? "Done cropping" : "Edit crops on the source"}
            </button>
          </div>
          <p class="muted small">
            {props.cropping
              ? "The preview shows the whole source. Drag a box to move its crop; drag a corner to resize it (its shape stays, so the panel keeps its size). Each part keeps its own crops."
              : "Drag a layer on the preview to move it on the frame; double-click the preview (or a layer's clip on the timeline) to edit crops on the source picture."}
          </p>
        </div>
        </Show>
      </Show>
    </section>
  );
}
