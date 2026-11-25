// Agents at work, drawn over the cut (decision 0059): one cursor per agent run, on the
// thing its last commit changed, with what it did and whose it is; a ring on each thing
// just changed and a dashed outline where one was removed. All of it comes from the
// project's own `committed` events (presence.ts): nothing moves that no commit moved.
// The layer takes no input and is hidden from assistive tech (the top bar's "Agent
// working on this cut" is the status that is read out).
import { createEffect, For, on, onCleanup } from "solid-js";
import "./agents.css";
import { cutView } from "../store";
import { cursors, knowRun, LABEL_MS, LINGER_MS, locate, marks, type Place } from "../presence";

/** Where a change that no pane draws as a thing is shown: the cut's title for the cut
 * itself; the viewer for what changes the picture as a whole (a framing, a layer, a
 * style). A part or an overlay that cannot be found is not here: it was removed since, and
 * its cursor stays where it last was. */
const PICTURE = new Set(["framing", "layer", "layout", "styles", "font", "caption_doc", "caption_presets", "look"]);
function elsewhere(entity: string): Place | null {
  const kind = entity.slice(0, entity.indexOf(":"));
  if (kind !== "cut" && !PICTURE.has(kind)) return null;
  const el = document.querySelector<HTMLElement>(kind === "cut" ? '[data-testid="cut-title"]' : ".viewer-canvas");
  const r = el?.getBoundingClientRect();
  if (!r || r.width <= 0) return null;
  return kind === "cut" ? { x: r.left, y: r.top, w: r.width, h: r.height } : { x: r.right - 36, y: r.top + 28, w: 0, h: 0 };
}

export function AgentLayer() {
  const cursorEls = new Map<string, HTMLDivElement>();
  const ringEls = new Map<string, HTMLDivElement>();
  /** Each cursor's last point: where it stays when its target cannot be placed. */
  const at = new Map<string, { x: number; y: number }>();
  let raf = 0;

  const frame = () => {
    raf = 0;
    const now = performance.now();
    // read every place first, then write (no layout between the two)
    const ringAt = marks.map((m) => ({ m, p: m.op === "delete" ? m.was : locate(m.entity) }));
    const curAt = cursors.map((c) => {
      const p = c.target ? locate(c.target) ?? c.pin ?? elsewhere(c.target) : null;
      return { c, p };
    });
    for (const { m, p } of ringAt) {
      const el = ringEls.get(m.key);
      if (!el) continue;
      if (!p || p.off || p.w < 2) {
        el.style.opacity = "0";
        continue;
      }
      el.style.transform = `translate3d(${Math.round(p.x)}px, ${Math.round(p.y)}px, 0)`;
      el.style.width = `${Math.round(p.w)}px`;
      el.style.height = `${Math.round(p.h)}px`;
      el.style.opacity = String(Math.max(0, Math.min(1, (m.until - now) / 500)));
    }
    for (const { c, p } of curAt) {
      const el = cursorEls.get(c.run);
      if (!el) continue;
      let pt = at.get(c.run);
      if (p) {
        const x = c.side === "in" ? p.x + 2 : c.side === "out" ? p.x + p.w - 2 : p.x + Math.min(p.w / 2, 44);
        pt = { x, y: p.y + (p.h ? p.h * 0.58 : 0) };
      }
      // (no commit placed yet: beside the top bar's "Agent working" chip, or the corner)
      if (!pt) {
        const chip = document.querySelector<HTMLElement>('[data-testid="agent-working"]')?.getBoundingClientRect();
        pt = chip ? { x: chip.left + 10, y: chip.bottom + 2 } : { x: innerWidth - 220, y: 56 };
      }
      at.set(c.run, pt);
      // kept inside the window, its tag included
      const x = Math.max(4, Math.min(innerWidth - 24, pt.x));
      const y = Math.max(4, Math.min(innerHeight - 40, pt.y));
      el.style.transform = `translate3d(${Math.round(x)}px, ${Math.round(y)}px, 0)`;
      const quiet = now - Math.max(c.editedAt, c.calledAt);
      const idle = now - c.editedAt > LABEL_MS;
      el.style.opacity = String(Math.max(0, Math.min(1, (LINGER_MS - quiet) / 600)));
      el.dataset.idle = idle ? "1" : "0";
      el.dataset.off = p?.off ?? "";
      el.dataset.flip = x > innerWidth - 190 ? "1" : "0";
    }
    if (cursors.length || marks.length) raf = requestAnimationFrame(frame);
  };
  createEffect(() => {
    // (the lists themselves: a new cursor or ring starts the loop again)
    if ((cursors.length || marks.length) && !raf) raf = requestAnimationFrame(frame);
  });
  onCleanup(() => cancelAnimationFrame(raf));

  // the cut's own word for who has a run open on it names that run's cursor
  createEffect(on(() => [cutView()?.agent_working?.run, cutView()?.agent_working?.identity, cursors.length] as const, ([run, identity]) => {
    if (run && identity) knowRun(run, identity);
  }));

  return (
    <div class="agent-layer" aria-hidden="true" data-testid="agent-layer" data-cursors={cursors.length} data-marks={marks.length}>
      <For each={marks}>
        {(m) => (
          <div
            class="agent-ring"
            classList={{ gone: m.op === "delete", made: m.op === "create" }}
            data-testid="agent-ring"
            data-entity={m.entity}
            data-op={m.op}
            style={{ "--agent-hue": String(m.hue) }}
            ref={(el) => {
              ringEls.set(m.key, el);
              onCleanup(() => ringEls.delete(m.key));
            }}
          />
        )}
      </For>
      <For each={cursors}>
        {(c) => (
          <div
            class="agent-cursor"
            data-testid="agent-cursor"
            data-run={c.run}
            data-name={c.name}
            data-label={c.label}
            data-target={c.target ?? ""}
            data-changes={c.changes}
            style={{ "--agent-hue": String(c.hue) }}
            ref={(el) => {
              cursorEls.set(c.run, el);
              onCleanup(() => {
                cursorEls.delete(c.run);
                at.delete(c.run);
              });
            }}
          >
            <svg class="agent-arrow" width="18" height="22" viewBox="0 0 18 22">
              <path d="M2 1.5 L2 17.2 L6.3 13.4 L9.2 20 L12.1 18.7 L9.3 12.3 L15 12.1 Z" />
            </svg>
            <span class="agent-tag">
              <b>{c.name}</b>
              <span class="agent-did">{c.label}</span>
            </span>
          </div>
        )}
      </For>
    </div>
  );
}
