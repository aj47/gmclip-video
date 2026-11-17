// The startup capability probe (R-PLAT-1, backlog H.09): what VideoToolbox (Media Foundation
// on Windows) offers on this machine, asked once at launch — H.264 play/export, VP9, and HEVC / AV1 as
// reported — each line with the exact fix where gmclip cannot do something. Collapsed
// when the Mac reads everything gmclip reads; open when a recording format cannot be read.
import { createResource, For, Show } from "solid-js";
import { actions } from "../actions";
import { thisMachineTitle } from "../platform";
import { attempt } from "../store";

export function MachineCaps() {
  const [caps] = createResource(async () => (await attempt(() => actions.capsStartup())) ?? null);
  const blocked = () => (caps()?.lines ?? []).filter((l) => l.level === "no").length;
  return (
    <Show when={caps()}>
      {(c) => (
        <section class="mac-caps" aria-label={thisMachineTitle}>
          <details open={blocked() > 0} data-testid="mac-caps">
            <summary class="small">
              {thisMachineTitle}: {c().machine.chip || (c().machine.windows ? "unknown GPU" : "unknown chip")}
              {blocked() ? ` · ${blocked()} recording format${blocked() === 1 ? "" : "s"} it cannot open` : " · plays and exports H.264"}
            </summary>
            <For each={c().lines}>
              {(l) => (
                <div class="cap-line" data-level={l.level} data-testid="mac-cap">
                  <span class="cap-dot" aria-hidden="true" />
                  <span>{l.text}</span>
                  <Show when={l.fix}><p class="fix">{l.fix}</p></Show>
                </div>
              )}
            </For>
          </details>
        </section>
      )}
    </Show>
  );
}
