// What an agent session is doing, drawn (decision 0035): a status line in plain words,
// the questions it asks, its plan, the cuts it touched (their pictures, parts before and
// after, Open), and its last message. The full log — every message, thought and tool
// call — is a power-user view behind "Show log". Shared by the Agents workspace and the
// floating agent chat in a project.
import { createEffect, createMemo, createResource, createSignal, For, Match, on, Show, Switch } from "solid-js";
import { actions, sessionActions, type SessionEntry, type SessionView } from "../actions";
import { Button } from "../design/components";
import { Icon } from "../design/Icon";
import { canPickUp, entries, isLive, loadSession } from "../agentsState";
import { attempt, openCut } from "../store";

type WorkItem = NonNullable<SessionView["work"]>[number];
type ToolEntry = Extract<SessionEntry, { type: "tool" }>;
type PermEntry = Extract<SessionEntry, { type: "permission" }>;

const SHOW_LOG = "gmclip.agents.showLog";
const [showLog, setShowLogRaw] = createSignal(localStorage.getItem(SHOW_LOG) === "1");
export { showLog };
export function setShowLog(on: boolean) {
  setShowLogRaw(on);
  localStorage.setItem(SHOW_LOG, on ? "1" : "0");
}

/** gmclip's tool names in plain words (what the status line says it is doing). */
const VERBS: [RegExp, string][] = [
  [/^project_list|^library/, "Looking through your projects"],
  [/transcript|words_read|caption_read/, "Reading the transcript"],
  [/^see\b|frame|look/, "Looking at the footage"],
  [/^listen|audio_read/, "Listening"],
  [/moment.*(add|set)|ai_pass|pass_step/, "Finding moments"],
  [/^moment/, "Going through moments"],
  [/cut_from|cut_new|cut_import|cut_like|longform_from|cut_create/, "Making a cut"],
  [/trim|range_delete|words_strike/, "Trimming"],
  [/split/, "Splitting a part"],
  [/part_delete|words_restore/, "Removing and restoring parts"],
  [/part_move|reorder|chronological/, "Reordering parts"],
  [/caption/, "Working on captions"],
  [/layout|crop|framing|layer/, "Framing the shot"],
  [/hook|opening|title|copy_draft|thumb/, "Writing the hook and titles"],
  [/export|render/, "Exporting"],
  [/verify/, "Checking the result"],
  [/cut_view|cut_get|cut_read|plan/, "Reading the cut"],
  [/cut_set/, "Updating the cut"],
];

/** A tool call as a sentence ("Trimming “Hook test”", "Running a command"). */
export function humanize(title: string, kind: string, gmclip: boolean): string {
  if (gmclip) {
    const name = title.replace(/^mcp__gmclip__|^gmclip[/._ ·]+/, "");
    for (const [re, said] of VERBS) if (re.test(name)) return said;
    const words = name.replace(/_/g, " ");
    return words.charAt(0).toUpperCase() + words.slice(1);
  }
  switch (kind) {
    case "execute":
      return "Running a command";
    case "read":
      return "Reading files";
    case "edit":
      return "Editing files";
    case "delete":
      return "Deleting files";
    case "search":
      return "Searching";
    case "fetch":
      return "Looking something up online";
    case "think":
      return "Thinking";
    default:
      return title.length > 60 ? `${title.slice(0, 60)}…` : title;
  }
}

/** The status line: what it is doing now, or whose turn it is. */
export function statusLine(s: SessionView): string {
  switch (s.state) {
    case "starting":
      return `Starting ${s.harness_label}…`;
    case "waiting":
      return "Needs your answer";
    case "working":
      return s.doing ? `${humanize(s.doing.title, s.doing.kind, s.doing.gmclip)}…` : "Thinking…";
    case "idle":
      return "Done — your turn";
    case "ended":
      return "Ended";
    case "failed":
      return s.error ?? "Failed";
  }
}

/** The smiley, alive while the agent works. */
export function AgentFace(props: { state: SessionView["state"] | "none"; size?: number }) {
  return (
    <span class={`ag-face ${props.state}`} aria-hidden="true">
      <Icon name="smile" size={props.size ?? 20} />
    </span>
  );
}

/** One cut the session touched: its picture strip, parts before → now, Open. */
export function WorkCard(props: { w: WorkItem; live: boolean }) {
  const [map] = createResource(
    () => props.w.project,
    async (p) => (await attempt(() => actions.projectMap(p))) ?? null,
  );
  const durMs = () => {
    const m = map();
    const st = (m?.streams ?? []).find((c) => c.cut === props.w.cut);
    if (st) return st.kept_ms;
    return [...(m?.longs ?? []), ...(m?.shorts ?? [])].find((c) => c.cut === props.w.cut)?.duration_ms ?? null;
  };
  const [strip] = createResource(
    () => ({ p: props.w.project, c: props.w.cut, parts: props.w.parts, last: props.w.last_ms, d: durMs() }),
    async (k) => {
      const frames = Math.max(30, Math.round(((k.d ?? 60_000) / 1000) * 30));
      return (await actions.filmstrip(k.p, k.c, frames / 6, 96).catch(() => null))?.thumbs.slice(0, 6) ?? null;
    },
  );
  let canvas!: HTMLCanvasElement;
  createEffect(() => {
    const t = (strip() ?? []).filter((x) => x.image);
    if (!canvas || !t.length) return;
    const first = t[0]!.image!;
    const h = 54;
    const w = Math.round((first.width / first.height) * h);
    canvas.width = w * t.length;
    canvas.height = h;
    const g = canvas.getContext("2d");
    t.forEach((x, i) => g?.drawImage(x.image!, i * w, 0, w, h));
  });
  const delta = () => props.w.parts - props.w.parts_before;
  const secs = () => {
    const d = durMs();
    return d === null ? null : d >= 60_000 ? `${Math.floor(d / 60_000)}:${String(Math.round((d % 60_000) / 1000)).padStart(2, "0")}` : `${(d / 1000).toFixed(1)} s`;
  };
  return (
    <div class={`ag-work${props.live ? " live" : ""}`} data-testid="work-card" data-cut={props.w.cut}>
      <div class="ag-work-strip">
        <canvas ref={canvas} hidden={!(strip() ?? []).some((x) => x.image)} />
        <Show when={!(strip() ?? []).some((x) => x.image)}>
          <Icon name="film" size={22} />
        </Show>
        <Show when={props.live}>
          <span class="ag-work-live">editing</span>
        </Show>
      </div>
      <div class="ag-work-body">
        <p class="ag-work-title" title={props.w.title}>
          <Show when={props.w.created}>
            <span class="ag-work-new">new</span>
          </Show>
          {props.w.title}
        </p>
        <p class="muted small">
          {props.w.parts} part{props.w.parts === 1 ? "" : "s"}
          <Show when={delta() !== 0 && !props.w.created}>
            <span class={delta() < 0 ? "ag-del" : "ag-add"}> ({delta() > 0 ? "+" : "−"}{Math.abs(delta())})</span>
          </Show>
          <Show when={secs()}> · {secs()}</Show>
          <Show when={props.w.calls}> · {props.w.calls} change{props.w.calls === 1 ? "" : "s"}</Show>
        </p>
        <p class="muted small ag-work-last">Last: {humanize(props.w.last_tool, "other", true).toLowerCase()}</p>
      </div>
      <Button size="sm" variant="ghost" iconAfter="chevron-right" data-testid="work-open" onClick={() => void openCut(props.w.project, props.w.cut)}>
        Open
      </Button>
    </div>
  );
}

/** A question the agent asks before running a tool. */
export function AskCard(props: { q: PermEntry; s: SessionView }) {
  return (
    <div class={`ag-ask${props.q.chosen ? " done" : ""}`} data-testid="entry-permission" role={props.q.chosen ? undefined : "alert"}>
      <p class="small">
        <Icon name="triangle-alert" size={13} /> {props.s.harness_label} wants to run <b>{props.q.title}</b>
      </p>
      <Show
        when={!props.q.chosen}
        fallback={<p class="muted small">{props.q.chosen === "cancelled" ? "Not answered (the turn ended)" : `You chose: ${props.q.options.find((o) => o.id === props.q.chosen)?.name ?? props.q.chosen}`}</p>}
      >
        <div class="row">
          <For each={props.q.options}>
            {(o) => (
              <Button
                size="sm"
                variant={o.kind === "allow_once" ? "primary" : o.kind.startsWith("reject") ? "secondary" : "ghost"}
                data-testid="permission-option"
                data-kind={o.kind}
                onClick={() => void attempt(() => sessionActions.answer(props.s.id, props.q.id, o.id))}
              >
                {o.name}
              </Button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

/** The plan as a checklist. */
export function PlanCard(props: { items: { content: string; status: string }[] }) {
  return (
    <div class="ag-plan" data-testid="entry-plan">
      <p class="ag-plan-head small">
        <Icon name="list-tree" size={13} /> Plan · {props.items.filter((i) => i.status === "completed").length}/{props.items.length}
      </p>
      <ol>
        <For each={props.items}>
          {(i) => (
            <li class={i.status}>
              <Icon name={i.status === "completed" ? "circle-check" : i.status === "in_progress" ? "loader" : "dot"} size={12} /> {i.content}
            </li>
          )}
        </For>
      </ol>
    </div>
  );
}

/** The session drawn (or its full log, behind the toggle). */
export function SessionBody(props: { s: SessionView; compact?: boolean }) {
  const s = () => props.s;
  let scroller!: HTMLDivElement;
  let stick = true;
  const onScroll = () => {
    stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;
  };
  createEffect(
    on(
      () => [entries().length, entries()[entries().length - 1], showLog(), s().work?.length] as const,
      () =>
        queueMicrotask(() => {
          if (stick && scroller) scroller.scrollTop = scroller.scrollHeight;
        }),
    ),
  );
  const asks = createMemo(() => entries().filter((e): e is PermEntry => e.type === "permission" && !e.chosen));
  const plan = createMemo(() => {
    const p = [...entries()].reverse().find((e) => e.type === "plan");
    return p && p.type === "plan" ? p.items : null;
  });
  /** The last thing it said (since the human's last message). */
  const said = createMemo(() => {
    const es = entries();
    const lastUser = es.map((e) => e.type).lastIndexOf("user");
    const a = [...es.slice(lastUser + 1)].reverse().find((e) => e.type === "agent");
    return a && a.type === "agent" ? a.text.trim() : "";
  });
  const asked = createMemo(() => {
    const es = entries();
    const u = [...es].reverse().find((e) => e.type === "user");
    return u && u.type === "user" ? u.text : s().title;
  });
  const notices = createMemo(() => entries().filter((e) => e.type === "notice").slice(-2));
  const tools = createMemo(() => entries().filter((e): e is ToolEntry => e.type === "tool"));
  const recent = createMemo(() => tools().filter((t) => !t.gmclip || !(s().work ?? []).length).slice(-3));
  const liveCut = () => {
    const w = s().work ?? [];
    if (s().state !== "working" || !w.length) return null;
    return [...w].sort((a, b) => b.last_ms - a.last_ms)[0]!.cut;
  };
  const [more, setMore] = createSignal(false);
  return (
    <div class={`ag-body${props.compact ? " compact" : ""}`} ref={scroller} onScroll={onScroll} data-testid="session-transcript" aria-live="polite">
      <Show
        when={!showLog()}
        fallback={
          <div class="ag-log" data-testid="session-log">
            <For each={entries()}>{(e) => <EntryView e={e} s={s()} />}</For>
          </div>
        }
      >
        <div class="ag-asked small" data-testid="session-asked">
          <span class="muted">You asked</span> {asked()}
        </div>
        <div class={`ag-now ${s().state}`} data-testid="session-now">
          <AgentFace state={s().state} size={props.compact ? 22 : 28} />
          <div class="ag-now-text">
            <p class="ag-now-line" data-testid="session-status">{statusLine(s())}</p>
            <Show when={tools().length}>
              <p class="muted small">{tools().length} step{tools().length === 1 ? "" : "s"} · {(s().work ?? []).length} cut{(s().work ?? []).length === 1 ? "" : "s"} touched</p>
            </Show>
          </div>
        </div>
        <For each={asks()}>{(q) => <AskCard q={q} s={s()} />}</For>
        <Show when={plan()}>{(p) => <PlanCard items={p()} />}</Show>
        <Show when={(s().work ?? []).length}>
          <div class="ag-works" data-testid="session-work">
            <For each={s().work ?? []}>{(w) => <WorkCard w={w} live={liveCut() === w.cut} />}</For>
          </div>
        </Show>
        <Show when={recent().length && isLive(s().state)}>
          <ul class="ag-steps small">
            <For each={recent()}>
              {(t) => (
                <li class={t.status}>
                  <Icon name={t.status === "completed" ? "circle-check" : t.status === "failed" ? "x" : "loader"} size={12} /> {humanize(t.title, t.kind, t.gmclip)}
                </li>
              )}
            </For>
          </ul>
        </Show>
        <For each={notices()}>{(n) => n.type === "notice" && <p class={`ag-notice small${n.error ? " err" : " muted"}`} data-testid="entry-notice">{n.text}</p>}</For>
        <Show when={said()}>
          <div class={`ag-said${more() ? " open" : ""}`} data-testid="entry-agent" aria-label={`From ${s().harness_label} (untrusted)`}>
            <p>{said()}</p>
            <Show when={said().length > 280}>
              <button type="button" class="ghost small" onClick={() => setMore(!more())}>{more() ? "Less" : "More"}</button>
            </Show>
          </div>
        </Show>
      </Show>
      <button type="button" class="ag-logtoggle ghost small" data-testid="session-log-toggle" aria-pressed={showLog()} onClick={() => setShowLog(!showLog())}>
        <Icon name="file-text" size={12} /> {showLog() ? "Hide log" : "Show log"}
      </button>
    </div>
  );
}

/** The box for the next message (Enter sends; Stop while it works). In an ended session
 * it picks the session up again with the message. */
export function Composer(props: { s: SessionView; compact?: boolean }) {
  const s = () => props.s;
  const [text, setText] = createSignal("");
  const busy = () => s().state === "working" || s().state === "waiting" || s().state === "starting";
  const send = async () => {
    const t = text().trim();
    if (!t || busy()) return;
    if (canPickUp(s())) {
      if (await attempt(() => sessionActions.resume(s().id, t))) {
        setText("");
        void loadSession(s().id);
      }
      return;
    }
    if (await attempt(() => sessionActions.prompt(s().id, t))) setText("");
  };
  return (
    <form
      class={`ag-composer${props.compact ? " compact" : ""}`}
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <textarea
        rows={props.compact ? 1 : 2}
        aria-label="Message to the agent"
        data-testid="session-input"
        placeholder={isLive(s().state) ? (busy() ? "It's working…" : "Tell it what next") : canPickUp(s()) ? "Ended. Send a message to pick it up again" : "This session has ended."}
        disabled={!isLive(s().state) && !canPickUp(s())}
        value={text()}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <Show
        when={busy() && s().state !== "starting"}
        fallback={
          <Button type="submit" variant="primary" icon="send" data-testid="session-send" disabled={!text().trim() || busy() || (!isLive(s().state) && !canPickUp(s()))}>
            {props.compact ? "" : "Send"}
          </Button>
        }
      >
        <Button variant="secondary" icon="x" data-testid="session-cancel" tooltip="Stops this turn; the session stays" onClick={() => void attempt(() => sessionActions.cancel(s().id))}>
          {props.compact ? "" : "Stop"}
        </Button>
      </Show>
    </form>
  );
}

const TOOL_ICON: Record<string, string> = {
  read: "book-open",
  edit: "pencil",
  delete: "x",
  move: "arrow-left",
  search: "search",
  execute: "command",
  think: "sparkles",
  fetch: "cloud",
};

export function EntryView(props: { e: SessionEntry; s: SessionView }) {
  return (
    <Switch>
      <Match when={props.e.type === "user" && props.e}>
        {(e) => (
          <div class="ag-msg ag-user" data-testid="entry-user">
            <p>{(e() as { text: string }).text}</p>
          </div>
        )}
      </Match>
      <Match when={props.e.type === "agent" && props.e}>
        {(e) => (
          <div class="ag-msg ag-agent" data-testid="entry-agent" aria-label={`From ${props.s.harness_label} (untrusted)`}>
            <p>{(e() as { text: string }).text}</p>
          </div>
        )}
      </Match>
      <Match when={props.e.type === "thought" && props.e}>
        {(e) => (
          <details class="ag-thought">
            <summary class="muted small">Thinking</summary>
            <p class="muted small">{(e() as { text: string }).text}</p>
          </details>
        )}
      </Match>
      <Match when={props.e.type === "tool" ? (props.e as Extract<SessionEntry, { type: "tool" }>) : null}>
        {(t) => (
          <details class={`ag-tool ${t().status}${t().gmclip ? " gm" : ""}`} data-testid="entry-tool" data-status={t().status}>
            <summary>
              <Icon name={t().gmclip ? "sparkles" : (TOOL_ICON[t().kind] ?? "sliders")} size={13} />
              <span class="ag-tool-title">{t().gmclip ? t().title.replace(/^mcp__gmclip__/, "gmclip · ") : t().title}</span>
              <span class={`ag-tool-status ${t().status}`}>{t().status === "in_progress" ? "running" : t().status}</span>
            </summary>
            <Show when={t().input}>{(inp) => <pre class="ag-pre">{inp().length > 4000 ? `${inp().slice(0, 4000)}… (${inp().length} characters; the whole input is kept in the session file)` : inp()}</pre>}</Show>
            <Show when={t().diffs.length}>
              <ul class="ag-diffs small">
                <For each={t().diffs}>{(d) => <li><code>{d.path}</code> <span class="ag-add">+{d.added}</span> <span class="ag-del">−{d.removed}</span></li>}</For>
              </ul>
            </Show>
            <Show when={t().locations.length && !t().diffs.length}>
              <p class="muted small file">{t().locations.join(", ")}</p>
            </Show>
            <Show when={t().detail}><pre class="ag-pre">{t().detail}</pre></Show>
          </details>
        )}
      </Match>
      <Match when={props.e.type === "plan" ? (props.e as Extract<SessionEntry, { type: "plan" }>) : null}>
        {(p) => (
          <div class="ag-plan" data-testid="entry-plan">
            <p class="ag-plan-head small"><Icon name="list-tree" size={13} /> Plan</p>
            <ol>
              <For each={p().items}>{(i) => <li class={i.status}><Icon name={i.status === "completed" ? "circle-check" : i.status === "in_progress" ? "loader" : "dot"} size={12} /> {i.content}</li>}</For>
            </ol>
          </div>
        )}
      </Match>
      <Match when={props.e.type === "permission" ? (props.e as Extract<SessionEntry, { type: "permission" }>) : null}>
        {(q) => (
          <div class={`ag-ask${q().chosen ? " done" : ""}`} data-testid="entry-permission" role={q().chosen ? undefined : "alert"}>
            <p class="small">
              <Icon name="triangle-alert" size={13} /> {props.s.harness_label} wants to run <b>{q().title}</b>
            </p>
            <Show
              when={!q().chosen}
              fallback={<p class="muted small">{q().chosen === "cancelled" ? "Not answered (the turn ended)" : `You chose: ${q().options.find((o) => o.id === q().chosen)?.name ?? q().chosen}`}</p>}
            >
              <div class="row">
                <For each={q().options}>
                  {(o) => (
                    <Button
                      size="sm"
                      variant={o.kind === "allow_once" ? "primary" : o.kind.startsWith("reject") ? "secondary" : "ghost"}
                      data-testid="permission-option"
                      data-kind={o.kind}
                      onClick={() => void attempt(() => sessionActions.answer(props.s.id, q().id, o.id))}
                    >
                      {o.name}
                    </Button>
                  )}
                </For>
              </div>
            </Show>
          </div>
        )}
      </Match>
      <Match when={props.e.type === "notice" ? (props.e as Extract<SessionEntry, { type: "notice" }>) : null}>
        {(n) => <p class={`ag-notice small${n().error ? " err" : " muted"}`} data-testid="entry-notice">{n().text}</p>}
      </Match>
    </Switch>
  );
}
