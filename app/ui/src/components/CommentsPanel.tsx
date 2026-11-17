// The Review tab (decision 0015; V.01, V.02, V.04, V.05): comments pinned to a moment and a
// box on the frame, threads with replies and resolve; suggested edits from an editor's
// review link, accepted or rejected one by one (each accept one ⌘Z); "Fix with my agent" on
// one comment or all open ones; the golden-cut switch and "Compare to golden".
// Text from review links and agents is shown as text only (R-UI-SEC-2).
import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, Show } from "solid-js";
import { actions, rate, type CommentThread, type GoldenCompare, type Suggestion } from "../actions";
import { Button, Chip, Empty, IconButton, Panel, Switch, tip } from "../design/components";
import { attempt, cutView, say, viewer } from "../store";
import { draftBox, drawing, pickedThread, setDraftBox, setDrawing, setPickedThread, setThreads } from "../collab";

function clock(s: number) {
  const m = Math.floor(s / 60);
  const r = (s - m * 60).toFixed(1).padStart(4, "0");
  return `${m}:${r}`;
}

/** Who wrote it, in words (identities, never prose). */
export function who(a: string) {
  if (a === "human") return "You";
  if (a.startsWith("review-link:")) {
    const [id, login] = a.slice(12).split("/");
    return login ? `${login} (link ${id!.slice(0, 6)})` : `Review link ${id!.slice(0, 6)}`;
  }
  if (a.startsWith("launched:") || a.startsWith("local:")) return "Your agent";
  return a;
}

export function CommentsPanel() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [data, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.comments(k.p, k.c))) ?? null);
  createEffect(() => setThreads(data()?.threads ?? []));
  onCleanup(() => {
    setDrawing(false);
    setDraftBox(null);
    setPickedThread(null);
  });
  // a review link writes without a commit on this page's actions: look again now and then
  const timer = setInterval(() => void refetch(), 5000);
  onCleanup(() => clearInterval(timer));
  const [text, setText] = createSignal("");
  const [showResolved, setShowResolved] = createSignal(false);
  const fps = () => rate(cutView()?.fps);
  const here = () => viewer()?.frame ?? 0;
  const p = () => cutView()!.project;
  const c = () => cutView()!.cut;
  const add = async () => {
    const t = text().trim();
    if (!t || !cutView()) return;
    const r = await attempt(() => actions.commentAdd(p(), c(), t, here(), draftBox()));
    if (r !== undefined) {
      setText("");
      setDraftBox(null);
      setDrawing(false);
      void refetch();
    }
  };
  const shown = createMemo(() => (data()?.threads ?? []).filter((t) => showResolved() || !t.resolved));
  const byId = createMemo(() => new Map((data()?.threads ?? []).map((t) => [t.id, t] as const)));
  const shownIds = createMemo(() => shown().map((t) => t.id), undefined, { equals: (a, b) => a.length === b.length && a.every((x, i) => x === b[i]) });
  const resolvedCount = () => (data()?.threads ?? []).filter((t) => t.resolved).length;
  const fixAll = async () => {
    const r = await attempt(() => actions.commentsFix(p(), c(), null));
    if (r) say(`Your agent is working on ${data()?.open ?? 0} comment${data()?.open === 1 ? "" : "s"}`);
  };
  const working = (id: string) => (data()?.agent_runs ?? []).some((r) => r.comments.includes(id));
  return (
    <div class="side-pane comments-panel" data-testid="comments-panel">
      <Panel
        label="Comments"
        title={<>Comments <Show when={data()?.open}><Chip tone="accent">{data()!.open}</Chip></Show></>}
        actions={
          <Button size="sm" icon="bot" data-testid="fix-all" tooltip="Each open comment becomes a task for your agent; one run you can undo" disabled={!data()?.open} onClick={() => void fixAll()}>
            Fix all with my agent
          </Button>
        }
      >
        <form
          class="comment-compose"
          onSubmit={(e) => {
            e.preventDefault();
            void add();
          }}
        >
          <input type="text" aria-label="New comment" data-testid="comment-text" placeholder={`Comment at ${clock(here() / fps())}`} maxLength={2000} value={text()} onInput={(e) => setText(e.currentTarget.value)} onKeyDown={(e) => e.stopPropagation()} />
          <IconButton icon="square-dashed" label={drawing() ? "Stop drawing" : "Draw a box on the frame"} pressed={drawing()} data-testid="draw-box" onClick={() => { setDrawing(!drawing()); if (drawing()) setDraftBox(null); }} />
          <Button type="submit" size="sm" variant="primary" icon="message-square" data-testid="comment-add" disabled={!text().trim()}>
            Comment
          </Button>
        </form>
        <Show when={draftBox()}>
          <span class="muted small" data-testid="box-set">Box drawn</span>
        </Show>
        <Show when={(data()?.threads ?? []).length} fallback={<Empty compact icon="message-square" title="No comments yet" />}>
          <ul class="threads" aria-label="Comment threads">
            {/* keyed by id: a refresh brings new objects, and a thread's DOM (a reply being
                typed, its focus) must outlive it */}
            <For each={shownIds()}>{(id) => <Show when={byId().get(id)}>{(t) => <Thread t={t()} working={working(id)} refetch={() => void refetch()} />}</Show>}</For>
          </ul>
          <Show when={resolvedCount()}>
            <Button size="sm" variant="ghost" onClick={() => setShowResolved(!showResolved())}>
              {showResolved() ? "Hide resolved" : `Show ${resolvedCount()} resolved`}
            </Button>
          </Show>
        </Show>
      </Panel>
      <Suggestions list={data()?.suggestions ?? []} refetch={() => void refetch()} />
      <Golden />
    </div>
  );
}

function Thread(props: { t: CommentThread; working: boolean; refetch: () => void }) {
  const [reply, setReply] = createSignal("");
  const v = () => cutView()!;
  const t = () => props.t;
  const seek = () => {
    setPickedThread(t().id);
    if (t().frame !== null) void attempt(() => actions.viewerSeek(t().frame!));
  };
  const send = async () => {
    if (!reply().trim()) return;
    if ((await attempt(() => actions.commentReply(v().project, v().cut, t().id, reply().trim()))) !== undefined) {
      setReply("");
      props.refetch();
    }
  };
  return (
    <li class="thread" classList={{ resolved: t().resolved, picked: pickedThread() === t().id }} data-testid="thread" data-thread={t().id}>
      <div class="thread-head">
        <Show when={t().frame !== null} fallback={<Chip tone="warning" tooltip="The moment it points at is cut out now">{t().pinned ? "not in the cut" : "whole cut"}</Chip>}>
          <button type="button" class="chip chip-accent time-chip" data-testid="thread-time" {...tip("Go there")} onClick={seek}>
            {clock(t().t ?? 0)}
          </button>
        </Show>
        <Show when={t().box}>
          <Chip icon="square-dashed" tooltip="Has a box on the frame">box</Chip>
        </Show>
        <span class="thread-author" classList={{ external: !t().human }}>{who(t().author)}</span>
        <span class="thread-actions">
          <Show when={props.working}>
            <Chip tone="accent" icon="loader">agent</Chip>
          </Show>
          <IconButton
            icon="bot"
            size="sm"
            label="Fix with my agent"
            data-testid="thread-fix"
            disabled={t().resolved || props.working}
            onClick={() => void attempt(() => actions.commentsFix(v().project, v().cut, [t().id])).then((r) => r && say("Your agent is on it"))}
          />
          <IconButton
            icon={t().resolved ? "restore" : "check"}
            size="sm"
            label={t().resolved ? "Reopen" : "Resolve"}
            data-testid="thread-resolve"
            onClick={() => void attempt(() => actions.commentResolve(v().project, t().id, !t().resolved)).then(props.refetch)}
          />
          <IconButton icon="x" size="sm" label="Delete the thread" onClick={() => void attempt(() => actions.commentDelete(v().project, t().id)).then(props.refetch)} />
        </span>
      </div>
      <p class="thread-text" classList={{ untrusted: !t().human }} data-testid="thread-text">{t().text}</p>
      <For each={t().replies}>
        {(r) => (
          <p class="thread-reply" classList={{ untrusted: !r.human }} data-testid="thread-reply">
            <b>{who(r.author)}</b> {r.text}
          </p>
        )}
      </For>
      <form
        class="thread-reply-form"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <input type="text" aria-label="Reply" placeholder="Reply" maxLength={2000} value={reply()} onInput={(e) => setReply(e.currentTarget.value)} onKeyDown={(e) => e.stopPropagation()} />
        <IconButton icon="send" size="sm" label="Send reply" disabled={!reply().trim()} onClick={() => void send()} />
      </form>
    </li>
  );
}

function Suggestions(props: { list: Suggestion[]; refetch: () => void }) {
  const open = () => props.list.filter((s) => s.state === "proposed");
  const v = () => cutView()!;
  const decide = async (ids: string[], state: "accepted" | "rejected") => {
    const r = await attempt(() => actions.proposalsDecide(v().project, ids, state));
    if (r) say(state === "accepted" ? `Applied — ⌘Z undoes it` : "Rejected");
    props.refetch();
  };
  const authors = () => [...new Set(open().map((s) => s.author))];
  return (
    <Show when={props.list.length}>
      <Panel label="Suggested edits" title={<>Suggested edits <Show when={open().length}><Chip tone="warning">{open().length}</Chip></Show></>}>
        <ul class="suggestions" aria-label="Suggested edits">
          <For each={props.list}>
            {(s) => (
              <li class={`suggestion ${s.state}`} data-testid="suggestion" data-state={s.state}>
                <div class="suggestion-head">
                  <Show when={s.frame !== null}>
                    <button type="button" class="chip chip-neutral time-chip" {...tip("Go there")} onClick={() => void attempt(() => actions.viewerSeek(s.frame!))}>
                      {clock(s.frame! / (rate(cutView()?.fps)))}
                    </button>
                  </Show>
                  <span class="suggestion-summary">{s.summary}</span>
                </div>
                <span class="muted small">{who(s.author)}</span>
                <Show when={s.note}>
                  <p class="thread-text untrusted">{s.note}</p>
                </Show>
                <Show when={s.state === "proposed"} fallback={<Chip tone={s.state === "accepted" ? "success" : "neutral"}>{s.state}</Chip>}>
                  <span class="row">
                    <Button size="sm" variant="primary" icon="check" data-testid="suggestion-accept" tooltip="Apply it (one ⌘Z undoes it)" onClick={() => void decide([s.id], "accepted")}>
                      Accept
                    </Button>
                    <Button size="sm" variant="ghost" icon="x" data-testid="suggestion-reject" onClick={() => void decide([s.id], "rejected")}>
                      Reject
                    </Button>
                  </span>
                </Show>
              </li>
            )}
          </For>
        </ul>
        <For each={authors().filter((a) => open().filter((s) => s.author === a).length > 1)}>
          {(a) => (
            <Button size="sm" variant="ghost" onClick={() => void decide(open().filter((s) => s.author === a).map((s) => s.id), "rejected")}>
              Reject all from {who(a)}
            </Button>
          )}
        </For>
      </Panel>
    </Show>
  );
}

function Golden() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [g, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.golden(k.p, k.c))) ?? null);
  const [cmp, setCmp] = createSignal<GoldenCompare | null>(null);
  createEffect(on(key, () => setCmp(null), { defer: true }));
  const fmt = (n: number | null) => (n === null ? "—" : Math.abs(n) >= 10 ? n.toFixed(0) : n.toFixed(1));
  return (
    <Panel
      label="Golden cuts"
      title={<>Golden cuts <Show when={g()?.goldens.length}><Chip>{g()!.goldens.length}</Chip></Show></>}
      actions={
        <Button size="sm" icon="ruler" data-testid="golden-compare" disabled={!g()?.goldens.filter((x) => x.cut !== cutView()?.cut).length} tooltip="This cut's length, pacing, layouts and captions beside your golden cuts'" onClick={() => void attempt(() => actions.goldenCompare(cutView()!.project, cutView()!.cut)).then((r) => r && setCmp(r))}>
          Compare
        </Button>
      }
    >
      <Switch
        testid="golden-switch"
        label="Golden cut"
        hint="A finished cut your next ones should look and move like; agents get its measurements in every brief"
        on={!!g()?.golden}
        onChange={(on) => void attempt(() => actions.goldenMark(cutView()!.project, cutView()!.cut, on)).then(() => refetch())}
      />
      <Show when={cmp()}>
        {(r) => (
          <div class="golden-compare" data-testid="golden-table">
            <Show when={r().outside.length} fallback={<Chip tone="success" icon="circle-check">Inside your golden band</Chip>}>
              <Chip tone="warning" icon="triangle-alert">Outside: {r().outside.join(", ")}</Chip>
            </Show>
            <table>
              <thead>
                <tr>
                  <th />
                  <th>This cut</th>
                  <th {...tip("The golden cuts' range (one golden: ±15 %)")}>Golden</th>
                </tr>
              </thead>
              <tbody>
                <For each={r().rows.filter((x) => x.status !== "n/a")}>
                  {(row) => (
                    <tr data-status={row.status}>
                      <th scope="row">{row.label}</th>
                      <td classList={{ off: row.status !== "in" }}>{fmt(row.mine)}</td>
                      <td>{row.band ? `${fmt(row.band[0])}–${fmt(row.band[1])}` : "—"}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
            <For each={r().goldens}>{(x) => <p class="muted small golden-line"><b>{x.title}</b> · {x.line}</p>}</For>
          </div>
        )}
      </Show>
    </Panel>
  );
}
