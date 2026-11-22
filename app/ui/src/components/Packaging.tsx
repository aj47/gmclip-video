// Packaging a cut (decision 0024):
// - HookLines: on-screen hook lines built on a number the clip measurably shows, one click
//   each in the Captions tab's hook field; each says where its number comes from.
// - Openings: up to three alternate openings of a short — sibling versions sharing
//   everything after the opening — side by side in a compare sheet: play, export, the
//   retention an export of each carries, and "Make main".
// - CopyDrafts: title/description drafts per platform from your agent, as proposals you
//   accept, edit or reject (the Thumbnail tab). Nothing here publishes anything.
// - ChaptersBar: a long-form's YouTube chapters from its topics, with Copy.
import { createResource, createSignal, For, Show } from "solid-js";
import { actions, packagingActions, type CopyDraftRow, type CopyPlatformName, type HookLine, type OpeningVersion } from "../actions";
import { Button, Chip, Empty, IconButton, Panel, Sheet } from "../design/components";
import { Icon } from "../design/Icon";
import { attempt, cutView, say } from "../store";
import { trSel } from "../transcriptState";

/** A line with `*word*` marked: the marked words drawn in the accent. */
export function Marked(props: { text: string }) {
  return (
    <For each={props.text.split(/(\*[^*]+\*)/)}>
      {(part) => (part.length > 2 && part.startsWith("*") && part.endsWith("*") ? <mark class="title-accent">{part.slice(1, -1)}</mark> : part)}
    </For>
  );
}

function secs(ms: number): string {
  const s = ms / 1000;
  return s < 60 ? `${s.toFixed(1)} s` : `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

/** Copies text to the clipboard; false when the webview refuses. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

const kindLabel = (k: HookLine["kind"]) => (k === "measured" ? "measured" : k === "count" ? "counted" : "said");

/** The Captions tab's suggested hook lines: one click puts a line on the cut (from frame 0,
 *  until the selected word or for `durMs`), recording where its numbers come from. */
export function HookLines(props: { until: () => number | null; durMs: () => number | null; onUsed: (text: string) => void }) {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [lines] = createResource(key, async (k) => (await attempt(() => packagingActions.hookLines(k.p, k.c))) ?? null);
  const use = async (l: HookLine) => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => packagingActions.hookUse(v.project, v.cut, l, props.until(), props.until() === null ? props.durMs() : null));
    if (r !== undefined) {
      props.onUsed(l.text);
      say("Hook set, with where its number comes from");
    }
  };
  return (
    <div class="hook-lines" data-testid="hook-lines">
      {/* ideas only when there are some: an empty list is one quiet line, the core's own
          note (for agents) in its tooltip */}
      <Show when={(lines()?.suggestions ?? []).length} fallback={<p class="muted small" data-testid="hook-lines-none" role="status" data-tip={lines()?.note ?? undefined}>{lines.loading ? "Reading the clip's numbers…" : "Hook ideas show here when the clip says or measures a number."}</p>}>
        <p class="muted small">Ideas from the clip (every number in them is said or measured):</p>
        <ul class="hook-options" aria-label="Suggested hook lines">
          <For each={lines()!.suggestions}>
            {(l) => (
              <li>
                <button type="button" class="hook-option" data-testid="hook-line" data-kind={l.kind} onClick={() => void use(l)}>
                  <span class="hook-line-text"><Marked text={l.text} /></span>
                  <span class="hook-line-source muted small">
                    <Chip tone={l.kind === "spoken" ? "neutral" : "accent"}>{kindLabel(l.kind)}</Chip> {l.source}
                  </span>
                  <Show when={l.warning}>{(w) => <span class="hook-line-warn small" data-testid="hook-line-warning">{w()}</span>}</Show>
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------------------
// Openings
// ---------------------------------------------------------------------------------------

/** The Captions tab's openings: the versions in a line each, adding one, and the compare sheet. */
export function Openings() {
  const key = () => {
    const v = cutView();
    return v && (v.kind ?? "short") === "short" ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [data, { refetch }] = createResource(key, async (k) => (await attempt(() => packagingActions.openings(k.p, k.c))) ?? null);
  const [moments] = createResource(key, async (k) => (await actions.moments(k.p).catch(() => null))?.moments ?? []);
  const [story] = createResource(key, async (k) => (await actions.story(k.p, k.c).catch(() => null))?.suggestions ?? []);
  const [comparing, setComparing] = createSignal(false);
  const [pick, setPick] = createSignal("");
  const alternates = () => (data()?.versions ?? []).filter((x) => !x.main);
  const full = () => alternates().length >= (data()?.max_alternates ?? 3);
  const add = async () => {
    const v = cutView();
    const p = pick();
    if (!v) return;
    let r: unknown;
    if (p === "words") {
      const s = trSel();
      if (!s) return;
      r = await attempt(() => packagingActions.openingFromWords(v.project, v.cut, s.lo, s.hi));
    } else if (p.startsWith("m:")) {
      r = await attempt(() => packagingActions.openingFromMoment(v.project, v.cut, p.slice(2)));
    } else if (p.startsWith("s:")) {
      r = await attempt(() => packagingActions.openingFromSuggestion(v.project, v.cut, p.slice(2)));
    }
    if (r !== undefined) {
      setPick("");
      say("Opening added: a version that shares the rest of the cut");
      void refetch();
    }
  };
  return (
    <div class="openings" data-testid="openings">
      <ul class="opening-list" aria-label="Versions">
        <For each={data()?.versions ?? []}>
          {(o) => (
            <li class="opening-row" data-testid="opening-row" data-main={String(o.main)}>
              <span class="opening-label">{o.label}</span>
              <Show when={o.main}><Chip tone="success">main</Chip></Show>
              <span class="muted small">{secs(o.opening_ms)} + rest = {secs(o.total_ms)}</span>
              <Show when={o.retention}>{(r) => <Chip tone="accent" tooltip="Retention at 3 s on this version's export">{r().at_3s.toFixed(0)}% at 3 s</Chip>}</Show>
            </li>
          )}
        </For>
      </ul>
      <div class="row">
        <select aria-label="New opening from" data-testid="opening-from" value={pick()} disabled={full()} onChange={(e) => setPick(e.currentTarget.value)}>
          <option value="">{full() ? "Three alternates: remove one to add" : "Add an opening from…"}</option>
          <option value="words" disabled={!trSel()}>the words selected in the transcript</option>
          <Show when={(moments() ?? []).length}>
            <optgroup label="A moment (another hook take)">
              <For each={moments() ?? []}>{(m) => <option value={`m:${m.id}`}>{(m.excerpt || m.quote).slice(0, 60)}</option>}</For>
            </optgroup>
          </Show>
          <Show when={(story() ?? []).length}>
            <optgroup label="A story suggestion">
              <For each={story() ?? []}>{(s) => <option value={`s:${s.id}`}>{s.quote.slice(0, 60)}</option>}</For>
            </optgroup>
          </Show>
        </select>
        <Button size="sm" icon="plus" data-testid="opening-add" disabled={!pick() || full()} onClick={() => void add()}>
          Add
        </Button>
        <span class="spacer" />
        <Button size="sm" icon="layers" data-testid="openings-compare" disabled={alternates().length === 0} tooltip="The versions side by side: play, export, retention, make one the main version" onClick={() => { void refetch(); setComparing(true); }}>
          Compare
        </Button>
      </div>
      <Show when={comparing() && data()}>
        <OpeningsSheet versions={data()!.versions} bodyMs={data()!.body_ms} onClose={() => setComparing(false)} onChanged={() => void refetch()} />
      </Show>
    </div>
  );
}

function OpeningsSheet(props: { versions: OpeningVersion[]; bodyMs: number; onClose: () => void; onChanged: () => void }) {
  const [playing, setPlaying] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal<string | null>(null);
  const [hookDraft, setHookDraft] = createSignal("");
  const v = () => cutView()!;
  const idOf = (o: OpeningVersion) => o.opening ?? "main";
  const play = async (o: OpeningVersion) => {
    const r = await attempt(() => packagingActions.openingPreview(v().project, v().cut, o.opening ?? null));
    if (r) {
      setPlaying(idOf(o));
      void attempt(() => actions.viewerPlay());
      say(`Playing “${o.label}” in the viewer`);
    }
  };
  const back = async () => {
    setPlaying(null);
    await attempt(() => actions.viewerLoad(v().project, v().cut, { keep: true }));
  };
  const close = () => {
    if (playing()) void back();
    props.onClose();
  };
  const choose = async (o: OpeningVersion) => {
    if (!o.opening) return;
    const r = await attempt(() => packagingActions.openingChoose(v().project, o.opening!));
    if (r !== undefined) {
      say(`“${o.label}” is the main version now · ⌘Z undoes`);
      if (playing()) await back();
      props.onChanged();
    }
  };
  const remove = async (o: OpeningVersion) => {
    if (!o.opening) return;
    if ((await attempt(() => packagingActions.openingDelete(v().project, o.opening!))) !== undefined) props.onChanged();
  };
  const exportIt = async (o: OpeningVersion) => {
    const r = o.opening
      ? await attempt(() => packagingActions.openingExport(v().project, v().cut, o.opening!))
      : await attempt(() => actions.exportStart(v().project, v().cut));
    if (r) say(`Exporting “${o.label}”: once it is posted, an agent can import its retention (retention_import)`);
  };
  const saveHook = async (o: OpeningVersion) => {
    if (!o.opening) return;
    if ((await attempt(() => packagingActions.openingSet(v().project, o.opening!, { hook_text: hookDraft() }))) !== undefined) {
      setEditing(null);
      props.onChanged();
    }
  };
  return (
    <Sheet
      label="Compare openings"
      testid="openings-sheet"
      class="openings-sheet"
      size="lg"
      onClose={close}
      sub={<>Every version shares the rest of the cut ({secs(props.bodyMs)}). Play one in the viewer, export it, make it the main version.</>}
      actions={
        <Show when={playing()}>
          <Button size="sm" variant="ghost" icon="arrow-left" data-testid="opening-back" onClick={() => void back()}>Back to the cut</Button>
        </Show>
      }
    >
        <div class="opening-columns" role="list">
          <For each={props.versions}>
            {(o) => (
              <section class="opening-col" role="listitem" aria-label={o.label} data-testid="opening-col" data-main={String(o.main)} classList={{ playing: playing() === idOf(o) }}>
                <div class="row">
                  <strong class="opening-label">{o.label}</strong>
                  <Show when={o.main} fallback={<Show when={o.origin === "agent"}><Chip icon="bot">agent</Chip></Show>}>
                    <Chip tone="success" icon="star">main</Chip>
                  </Show>
                </div>
                <p class="muted small">Opening {secs(o.opening_ms)} · whole {secs(o.total_ms)}</p>
                <p class="opening-quote" data-testid="opening-quote">“{o.quote || "…"}”</p>
                <div class="opening-hook">
                  <span class="muted small">Hook{o.own_hook ? "" : " (the cut's)"}:</span>{" "}
                  <Show when={editing() === idOf(o)} fallback={<span data-testid="opening-hook"><Marked text={o.hook_text ?? "none"} /></span>}>
                    <input type="text" aria-label={`Hook for ${o.label}`} value={hookDraft()} maxLength={120} onInput={(e) => setHookDraft(e.currentTarget.value)} onKeyDown={(e) => e.stopPropagation()} />
                    <Button size="sm" onClick={() => void saveHook(o)}>Save</Button>
                  </Show>
                  <Show when={!o.main && editing() !== idOf(o)}>
                    <IconButton icon="pencil" label={`Edit the hook of ${o.label}`} size="sm" onClick={() => { setHookDraft(o.hook_text ?? ""); setEditing(idOf(o)); }} />
                  </Show>
                </div>
                <div class="opening-retention" data-testid="opening-retention">
                  <Show
                    when={o.retention}
                    fallback={<span class="muted small">{o.exports ? "Exported; no retention imported on it yet." : "No retention yet: export this version, post it, and have an agent import its curve."}</span>}
                  >
                    {(r) => (
                      <>
                        <span><b>{r().at_3s.toFixed(0)}%</b> at 3 s</span>
                        <span><b>{r().at_opening_end.toFixed(0)}%</b> at the opening's end</span>
                        <span><b>{r().at_end.toFixed(0)}%</b> at the end</span>
                        <span class="muted small">{r().platform}</span>
                      </>
                    )}
                  </Show>
                </div>
                <div class="row opening-actions">
                  <Button size="sm" icon="play" data-testid="opening-play" onClick={() => void play(o)}>Play</Button>
                  <Button size="sm" icon="upload" data-testid="opening-export" onClick={() => void exportIt(o)}>Export</Button>
                  <Show when={!o.main}>
                    <Button size="sm" variant="primary" icon="star" data-testid="opening-choose" tooltip="Its opening becomes the cut's; the current one stays a version" onClick={() => void choose(o)}>Make main</Button>
                    <IconButton icon="x" label={`Remove ${o.label}`} size="sm" onClick={() => void remove(o)} />
                  </Show>
                </div>
              </section>
            )}
          </For>
        </div>
    </Sheet>
  );
}

// ---------------------------------------------------------------------------------------
// Title / description drafts
// ---------------------------------------------------------------------------------------

const PLATFORMS: { id: CopyPlatformName; label: string }[] = [
  { id: "youtube", label: "YouTube" },
  { id: "shorts", label: "Shorts" },
  { id: "tiktok", label: "TikTok" },
  { id: "instagram", label: "Instagram" },
];

/** The Thumbnail tab's titles and descriptions: your agent's drafts per platform, each
 *  accepted, edited or rejected here. Nothing is published. */
export function CopyDrafts() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [data, { refetch }] = createResource(key, async (k) => (await attempt(() => packagingActions.drafts(k.p, k.c))) ?? null);
  const [editing, setEditing] = createSignal<string | null>(null);
  const [t, setT] = createSignal("");
  const [d, setD] = createSignal("");
  const v = () => cutView()!;
  const of = (p: CopyPlatformName) => (data()?.drafts ?? []).filter((x) => x.platform === p);
  const decide = async (x: CopyDraftRow, patch: Parameters<typeof packagingActions.draftDecide>[2]) => {
    if ((await attempt(() => packagingActions.draftDecide(v().project, x.id, patch))) !== undefined) {
      setEditing(null);
      void refetch();
    }
  };
  const draft = async () => {
    const r = await attempt(() => packagingActions.draftWithAgent(v().project, v().cut));
    if (r) say("Your agent is drafting a title and description per platform");
  };
  return (
    <Panel
      label="Titles and descriptions"
      testid="copy-drafts"
      title={<>Titles and descriptions</>}
      actions={
        <>
          <IconButton icon="restore" label="Refresh drafts" size="sm" onClick={() => void refetch()} />
          <Button size="sm" icon="bot" data-testid="copy-agent" tooltip="Your agent drafts a title and description per platform from the cut's transcript; you accept, edit or reject them. Nothing is posted." onClick={() => void draft()}>
            Draft with my agent
          </Button>
        </>
      }
    >
      <Show
        when={(data()?.drafts ?? []).length}
        fallback={
          <Show when={!data.loading} fallback={<p class="muted small" role="status">Reading the drafts…</p>}>
            <Empty compact icon="type" title="No drafts yet" testid="copy-empty">Draft with my agent writes a title and description per platform for you to accept, edit or reject. Nothing is posted.</Empty>
          </Show>
        }
      >
        <For each={PLATFORMS}>
          {(pl) => (
            <Show when={of(pl.id).length}>
              <section class="copy-platform" aria-label={pl.label} data-testid="copy-platform" data-platform={pl.id}>
                <h4>{pl.label}</h4>
                <For each={of(pl.id)}>
                  {(x) => (
                    <div class="copy-draft" classList={{ accepted: x.state === "accepted", rejected: x.state === "rejected" }} data-testid="copy-draft" data-state={x.state}>
                      <Show
                        when={editing() === x.id}
                        fallback={
                          <>
                            <p class="copy-title">{x.title}</p>
                            <p class="copy-desc">{x.description}</p>
                          </>
                        }
                      >
                        <input type="text" aria-label="Title" value={t()} onInput={(e) => setT(e.currentTarget.value)} onKeyDown={(e) => e.stopPropagation()} />
                        <textarea aria-label="Description" rows={4} value={d()} onInput={(e) => setD(e.currentTarget.value)} onKeyDown={(e) => e.stopPropagation()} />
                      </Show>
                      <div class="row">
                        <Chip tone={x.state === "accepted" ? "success" : x.state === "rejected" ? "danger" : "neutral"}>{x.state}</Chip>
                        <Show when={x.origin === "agent"}><Chip icon="bot">{x.edited ? "agent, edited" : "agent"}</Chip></Show>
                        <span class="spacer" />
                        <Show
                          when={editing() === x.id}
                          fallback={
                            <>
                              <Show when={x.state !== "accepted"}>
                                <Button size="sm" variant="primary" icon="check" data-testid="copy-accept" onClick={() => void decide(x, { state: "accepted" })}>Accept</Button>
                              </Show>
                              <IconButton icon="pencil" label="Edit" size="sm" onClick={() => { setT(x.title); setD(x.description); setEditing(x.id); }} />
                              <Show when={x.state !== "rejected"}>
                                <IconButton icon="x" label="Reject" size="sm" onClick={() => void decide(x, { state: "rejected" })} />
                              </Show>
                              <IconButton icon="copy" label="Copy title and description" size="sm" onClick={() => void copyText(`${x.title}\n\n${x.description}`).then((ok) => say(ok ? "Copied" : "Could not copy", ok ? "info" : "error"))} />
                            </>
                          }
                        >
                          <Button size="sm" variant="primary" data-testid="copy-save" onClick={() => void decide(x, { title: t(), description: d() })}>Save and accept</Button>
                          <Button size="sm" variant="ghost" onClick={() => setEditing(null)}>Cancel</Button>
                        </Show>
                      </div>
                    </div>
                  )}
                </For>
              </section>
            </Show>
          )}
        </For>
      </Show>
    </Panel>
  );
}

// ---------------------------------------------------------------------------------------
// Chapters
// ---------------------------------------------------------------------------------------

/** A long-form's YouTube chapters (from its topics, on its own timeline) with Copy. */
export function ChaptersBar() {
  const key = () => {
    const v = cutView();
    return v && v.kind === "long" ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [ch] = createResource(key, async (k) => (await packagingActions.chapters(k.p, k.c).catch(() => null)) ?? null);
  const [open, setOpen] = createSignal(false);
  return (
    <Show when={key() && ch()}>
      {(c) => (
        <div class="chapters-bar" role="status" data-testid="chapters-bar" data-valid={String(c().valid)}>
          <span class="chapters-sum">
            <Icon name="list-tree" size={14} />
            Chapters <b>{c().youtube_chapters.length}</b>
            <Show when={!c().valid}>
              <span class="muted small" data-testid="chapters-why-not"> — {c().why_not}</span>
            </Show>
          </span>
          <Button size="sm" variant="ghost" icon={open() ? "chevron-down" : "chevron-right"} data-testid="chapters-show" aria-expanded={open()} onClick={() => setOpen(!open())}>{open() ? "Hide" : "Show"}</Button>
          <Button
            size="sm"
            variant={c().valid ? "primary" : "secondary"}
            icon="copy"
            data-testid="chapters-copy"
            disabled={!c().youtube}
            tooltip="Copy the 00:00 lines for the YouTube description"
            onClick={() => void copyText(c().youtube).then((ok) => say(ok ? `Chapters copied (${c().youtube_chapters.length})` : "Could not copy", ok ? "info" : "error"))}
          >
            Copy for YouTube
          </Button>
          <Show when={open()}>
            <pre class="chapters-lines" data-testid="chapters-lines">{c().youtube}</pre>
            <For each={c().notes}>{(n) => <p class="muted small">{n}</p>}</For>
          </Show>
        </div>
      )}
    </Show>
  );
}
