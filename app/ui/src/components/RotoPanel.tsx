// Cut-outs and the activity crop for the part under the playhead (R.01–R.03): cut the
// person out of a layer with no green screen, see the matte alone, fix its edges with a
// brush, find bad parts by score, build the accurate mattes, and follow the action with
// the screen crop. Plain structure: the design system restyles it.
import { createSignal, For, Show } from "solid-js";
import { actions, type MatteBuild, type MatteRow, type PartView } from "../actions";
import { brushFeather, brushMode, brushOn, brushSize, setBrushFeather, setBrushMode, setBrushOn, setBrushSize } from "../roto";
import { isWindows } from "../platform";
import { attempt, cutView, viewer } from "../store";

export function RotoPanel(props: { part: PartView }) {
  const [rows, setRows] = createSignal<MatteRow[] | null>(null);
  const [build, setBuild] = createSignal<MatteBuild | null>(null);
  const [follow, setFollow] = createSignal<string | null>(null);
  const [confirmDraft, setConfirmDraft] = createSignal(false);
  const layers = () => props.part.layers.filter((l) => l.model);
  const matted = () => props.part.layers.some((l) => l.matte);
  const v = () => cutView();

  const refresh = async () => {
    const c = v();
    if (!c) return;
    const r = await attempt(() => actions.matteStatus(c.project, c.cut));
    if (r) {
      setRows(r.parts);
      setBuild(r.build);
    }
  };
  const startBuild = async () => {
    const c = v();
    if (!c) return;
    const r = await attempt(() => actions.matteBuild(c.project, c.cut));
    if (r) setBuild(r.build);
    // progress: poll while it runs
    const tick = async () => {
      await refresh();
      if (build()?.state === "running") setTimeout(() => void tick(), 1000);
      else void attempt(() => actions.viewerSeek(viewer()?.frame ?? 0));
    };
    setTimeout(() => void tick(), 1000);
  };
  const runFollow = async (part?: string) => {
    const c = v();
    if (!c) return;
    const r = await attempt(() => actions.followAction(c.project, c.cut, part));
    if (r) {
      const n = (r.parts as unknown[]).length;
      setFollow(n ? `Re-cropped ${n} part(s)${r.splits ? `, ${r.splits} split(s) where the action moves` : ""}.` : `Nothing to follow: ${r.skipped.map((s) => s.why).join("; ") || "no screen layer"}.`);
    }
  };

  return (
    <section class="roto" aria-label="Cut-out and follow the action" data-testid="roto-panel">
      <h3>Cut-out</h3>
      {/* Windows has no person segmentation (Apple Vision makes the cut-outs): one made on a
          Mac plays and exports here, and can be turned off */}
      <Show when={isWindows}>
        <p class="muted small" data-testid="roto-unavailable">Cutting a person out needs a Mac (Apple Vision). Cut-outs made there play and export here.</p>
      </Show>
      <Show when={viewer()?.draft_matte}>
        <p class="badge draft-matte" data-testid="draft-matte" title="This frame shows a quick draft cut-out. Build accurate mattes for the export's quality.">Draft matte</p>
      </Show>
      <For each={layers()}>
        {(l) => (
          <div class="row" role="group" aria-label={`Cut-out of ${l.role}`}>
            <label class="check" title={isWindows && !l.matte ? "Cutting a person out needs a Mac (Apple Vision)" : "Cut the person out of this layer with no green screen (Apple Vision), on every part that shows it"}>
              <input
                type="checkbox"
                data-testid={`roto-${l.role}`}
                checked={!!l.matte}
                disabled={isWindows && !l.matte}
                onChange={(e) => {
                  const c = v();
                  if (c && l.model) void attempt(() => actions.matteSet(c.project, l.model!, e.currentTarget.checked));
                }}
              />
              Cut out {l.role}
            </label>
            <Show when={l.matte}>
              <label class="num" title="Soften the cut-out's edge (source pixels)">
                <span>soften</span>
                <input
                  type="number"
                  min="0"
                  max="32"
                  step="1"
                  value={l.matte?.soften ?? 0}
                  onChange={(e) => {
                    const c = v();
                    const s = Number(e.currentTarget.value);
                    if (c && l.model && Number.isFinite(s)) void attempt(() => actions.matteSet(c.project, l.model!, true, Math.min(32, Math.max(0, s))));
                  }}
                />
              </label>
              <span class="muted small" title="Brush strokes on this part">{l.matte?.strokes?.length ?? 0} fix(es)</span>
            </Show>
          </div>
        )}
      </For>
      <Show when={matted()}>
        <div class="row">
          <label class="check" title="Show only the cut-out: white where the person is kept">
            <input type="checkbox" data-testid="matte-view" checked={viewer()?.matte_view ?? false} onChange={(e) => void attempt(() => actions.viewerMatteView(e.currentTarget.checked))} />
            Matte view
          </label>
          <label class="check" title="Paint on the frame to fix this part's cut-out edges">
            <input type="checkbox" data-testid="brush" checked={brushOn()} onChange={(e) => setBrushOn(e.currentTarget.checked)} />
            Fix edges (brush)
          </label>
        </div>
        <Show when={brushOn()}>
          <div class="row" role="group" aria-label="Brush">
            <button type="button" class={brushMode() === "add" ? "primary" : ""} title="Add to the person" onClick={() => setBrushMode("add")}>Add</button>
            <button type="button" class={brushMode() === "subtract" ? "primary" : ""} title="Take away from the person" onClick={() => setBrushMode("subtract")}>Subtract</button>
            <label class="num" title="Brush size (share of the picture height)">
              <span>size</span>
              <input type="range" min="0.005" max="0.08" step="0.005" value={brushSize()} onInput={(e) => setBrushSize(Number(e.currentTarget.value))} />
            </label>
            <label class="num" title="Soft brush edge">
              <span>feather</span>
              <input type="range" min="0" max="1" step="0.1" value={brushFeather()} onInput={(e) => setBrushFeather(Number(e.currentTarget.value))} />
            </label>
            <button
              type="button"
              class="ghost"
              title="Remove this part's brush fixes"
              onClick={() => {
                const c = v();
                const l = props.part.layers.find((x) => x.matte && x.model);
                if (c && l?.model && c.framing?.active) void attempt(() => actions.matteBrush(c.project, l.model!, props.part.id, c.framing!.active!, [], true));
              }}
            >
              Clear fixes
            </button>
          </div>
        </Show>
        <div class="row">
          <button type="button" data-testid="matte-quality" title="Score every part's cut-out so the bad ones are found fast" onClick={() => void refresh()}>Check quality</button>
          <button type="button" data-testid="matte-build" title="Accurate cut-outs build on their own in the background (about 11 frames a second; ticks on the timeline); this starts it now or shows how far it is" onClick={() => void startBuild()}>
            Build accurate mattes
          </button>
          <Show when={build()}>
            {(b) => <span class="muted small" data-testid="matte-build-state">{b().state === "running" ? `${b().done} / ${b().total} frames` : b().state === "failed" ? `failed: ${b().error}` : "accurate mattes ready"}</span>}
          </Show>
        </div>
        <Show when={rows()}>
          <ol class="matte-rows" data-testid="matte-rows">
            <For each={rows()!}>
              {(r) => (
                <li>
                  <button type="button" class="link" title="Go to this part" onClick={() => r.out && void attempt(() => actions.viewerSeek(r.out![0]))}>
                    {r.plan_part} · {r.layer}
                  </button>{" "}
                  <span class={r.quality && r.quality.score < 70 ? "warn" : "muted"}>
                    {r.quality ? `${r.quality.score.toFixed(0)}/100 · unsure ${(r.quality.uncertain * 100).toFixed(1)}% · flicker ${(r.quality.flicker * 100).toFixed(1)}%${r.quality.empty_frames ? ` · lost ${r.quality.empty_frames} frame(s)` : ""}` : "not built yet"}
                  </span>
                </li>
              )}
            </For>
          </ol>
        </Show>
        <div class="row">
          <Show when={!confirmDraft()} fallback={
            <>
              <span class="muted small">Export now with draft cut-outs where accurate ones are not built?</span>
              <button type="button" data-testid="export-draft-confirm" onClick={() => { const c = v(); setConfirmDraft(false); if (c) void attempt(() => actions.exportStart(c.project, c.cut, true)); }}>Export with draft mattes</button>
              <button type="button" class="ghost" onClick={() => setConfirmDraft(false)}>Cancel</button>
            </>
          }>
            <button type="button" class="ghost" title="Export without waiting for accurate cut-outs (the export record lists the draft ones)" onClick={() => setConfirmDraft(true)}>Export with draft mattes…</button>
          </Show>
        </div>
      </Show>
      <h3>Screen crop</h3>
      <div class="row">
        <button type="button" data-testid="follow-part" title="Crop the screen onto whatever is changing in this part (splits it where the action moves)" onClick={() => void runFollow(props.part.id)}>Follow the action</button>
        <button type="button" class="ghost" data-testid="follow-cut" title="Follow the action in every part" onClick={() => void runFollow()}>All parts</button>
      </div>
      <Show when={follow()}>
        <p class="muted small" data-testid="follow-result">{follow()}</p>
      </Show>
    </section>
  );
}
