// First-run tips: the how-to sentences that used to sit in the chrome show ONCE, as a
// small floating card over the viewer that takes no layout space, lets clicks through,
// and goes at the first click or key (or after 12 s). It is marked seen as soon as it
// shows, so it never comes back; the same words live in tooltips and the ? keys list.
// Which tips were seen is an app setting (ui-prefs.json `coach_seen`).
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { IconButton } from "../design/components";
import { Icon } from "../design/Icon";
import { coachSeen, markCoachSeen } from "../prefs";

export function Coach(props: { id: string; children: JSX.Element; class?: string }) {
  // decided once, when it mounts: seen before → never shown
  const [show, setShow] = createSignal(!coachSeen(props.id));
  onMount(() => {
    if (!show()) return;
    markCoachSeen(props.id);
    const go = () => setShow(false);
    const t = setTimeout(go, 12_000);
    // the first deliberate action dismisses it (after a beat, so the click that opened
    // the view does not)
    const arm = setTimeout(() => {
      addEventListener("pointerdown", go, true);
      addEventListener("keydown", go, true);
    }, 600);
    onCleanup(() => {
      clearTimeout(t);
      clearTimeout(arm);
      removeEventListener("pointerdown", go, true);
      removeEventListener("keydown", go, true);
    });
  });
  return (
    <Show when={show()}>
      <div class={`coach coach-float${props.class ? ` ${props.class}` : ""}`} role="note" data-testid={`coach-${props.id}`}>
        <Icon name="sparkles" size={14} />
        <p>{props.children}</p>
        <IconButton icon="x" size="sm" label="Got it" onClick={() => setShow(false)} />
      </div>
    </Show>
  );
}
