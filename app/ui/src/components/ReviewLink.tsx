// The review link (U.10; SPEC §5.14.1): serve this library's exports to a phone or a
// reviewer, links made and revoked here. Serving, a new link, the tailnet mode and the
// remote-fix allowance each go through the native sheet; a new link's URL is shown once.
import { thisMachine, thisMachineTitle } from "../platform";
import { createEffect, createResource, createSignal, For, Show } from "solid-js";
import { actions } from "../actions";
import { attempt, home, say } from "../store";
import { Button } from "../design/components";
import { qrDraw, qrEncode } from "../qr";

/** A new link as a QR code, for the phone's camera (the URL is shown once, so is this). */
function LinkQr(props: { url: string }) {
  let c: HTMLCanvasElement | undefined;
  createEffect(() => {
    const u = props.url;
    if (c) qrDraw(c, qrEncode(u), 4);
  });
  return <canvas ref={c} class="review-qr" role="img" aria-label="QR code of the new link" data-testid="review-qr" />;
}

export function ReviewLink() {
  const [st, { refetch }] = createResource(async () => (await attempt(() => actions.reviewStatus())) ?? null);
  const [fresh, setFresh] = createSignal<string | null>(null);
  const [host, setHost] = createSignal("");
  const [login, setLogin] = createSignal("");
  const [usd, setUsd] = createSignal("2");
  const [runs, setRuns] = createSignal("3");
  const [sends, setSends] = createSignal("2");
  const act = async (f: () => Promise<unknown>, done?: string) => {
    const r = await attempt(f);
    if (r !== undefined && done) say(done);
    void refetch();
    return r;
  };
  const tailnet = () => {
    const m = st()?.mode;
    return m && typeof m === "object" ? m.tailnet : null;
  };
  const live = () => (st()?.links ?? []).filter((l) => l.live);
  // R-REV-4: what links asked for that waits for a click here (Home → Needs you)
  const asks = () => home().needs_you.filter((n) => n.ask);
  const fixes = () => asks().filter((n) => n.action === "remote_fix").length;
  const when = (ms: number) => new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  return (
    <section class="review-link" aria-label="Review link">
      <h2>Review link</h2>
      <p class="muted small" data-testid="review-state">
        {st()?.serving ? `Serving at ${st()!.serving}` : "Not serving"} · {tailnet() ? `tailnet ${tailnet()!.host}` : `${thisMachine} only`}
      </p>
      <div class="row">
        <Show
          when={st()?.serving}
          fallback={<button type="button" data-testid="review-serve" onClick={() => void act(() => actions.reviewServe(true), "Serving review links")}>Serve review links</button>}
        >
          <button type="button" class="ghost" data-testid="review-stop" onClick={() => void act(() => actions.reviewServe(false), "Stopped serving")}>Stop serving</button>
          <button type="button" data-testid="review-new" onClick={() => void act(async () => {
            const r = await actions.reviewLinkNew();
            setFresh(r.url);
            return r;
          })}>New link</button>
          <Button size="sm" icon="pencil" data-testid="review-new-editor" tooltip="For an outside editor: comments and suggested edits only (you accept or reject each); no agent runs, no approvals" onClick={() => void act(async () => {
            const r = await actions.reviewLinkRole("editor");
            setFresh(r.url);
            return r;
          })}>Editor link</Button>
        </Show>
      </div>
      <Show when={fresh()}>
        {(u) => (
          <div class="row small fresh-link" data-testid="review-url">
            <input type="text" readOnly aria-label="New review link (shown once)" value={u()} onFocus={(e) => e.currentTarget.select()} />
            <button type="button" class="ghost" onClick={() => { void navigator.clipboard?.writeText(u()); say("Link copied"); }}>Copy</button>
            <button type="button" class="ghost" aria-label="Hide the link" onClick={() => setFresh(null)}>✕</button>
            <LinkQr url={u()} />
          </div>
        )}
      </Show>
      <Show when={asks().length}>
        <p class="small note" role="status" data-testid="review-asks">
          {asks().length === 1 ? "1 request" : `${asks().length} requests`} from links {asks().length === 1 ? "waits" : "wait"} for your click under Needs you
          {fixes() ? ` (${fixes()} “Fix with agent”)` : ""}.
        </p>
      </Show>
      <ul class="links" aria-label="Review links">
        <For each={live()} fallback={<li class="muted small">No live links.</li>}>
          {(l) => (
            <li class="row small" data-testid="review-link-row">
              <span>{l.id.slice(0, 8)} · until {when(l.expires_ms)}{l.login ? ` · ${l.login}` : ""}{l.role === "editor" ? " · editor" : ""}</span>
              <button type="button" class="ghost" aria-label={`Revoke link ${l.id.slice(0, 8)}`} onClick={() => void act(() => actions.reviewLinkRevoke(l.id), "Link revoked")}>Revoke</button>
            </li>
          )}
        </For>
      </ul>
      <Show when={live().length > 1}>
        <button type="button" class="ghost small" onClick={() => void act(() => actions.reviewLinkRevoke(null), "Every link revoked")}>Revoke all</button>
      </Show>
      <details>
        <summary class="small">Where links open, and remote fixes</summary>
        <form class="row small" onSubmit={(e) => { e.preventDefault(); void act(() => actions.ownerRequest("review_tailnet", { host: host().trim(), owner_login: login().trim() }), "Tailnet mode on"); }}>
          <input type="text" aria-label="Tailnet host" placeholder="mac.tailnet.ts.net" value={host()} onInput={(e) => setHost(e.currentTarget.value)} />
          <input type="text" aria-label="Your tailnet login" placeholder="you@example.com" value={login()} onInput={(e) => setLogin(e.currentTarget.value)} />
          <button type="submit" disabled={!host().trim() || !login().trim()}>Use my tailnet</button>
        </form>
        <Show when={tailnet()}>
          <button type="button" class="ghost small" onClick={() => void act(() => actions.ownerRequest("review_loopback", {}), `Links open on ${thisMachine} only`)}>{thisMachineTitle} only</button>
        </Show>
        <form class="row small" onSubmit={(e) => { e.preventDefault(); void act(() => actions.ownerRequest("remote_fix_set", { max_usd_per_run: Number(usd()), max_runs_per_day: Number(runs()) }), "Remote fixes allowed"); }}>
          <span class="muted">Remote “Fix with agent”:</span>
          <input class="num" type="number" min="0.01" step="0.5" aria-label="Most $ per run" value={usd()} onInput={(e) => setUsd(e.currentTarget.value)} />
          <span class="muted">$ a run,</span>
          <input class="num" type="number" min="1" max="50" aria-label="Runs per day" value={runs()} onInput={(e) => setRuns(e.currentTarget.value)} />
          <span class="muted">a day</span>
          <button type="submit">Allow</button>
          <Show when={st()?.remote_fix}>
            <button type="button" class="ghost" onClick={() => void act(() => actions.ownerRequest("remote_fix_set", { off: true }), "Remote fixes off")}>Turn off</button>
          </Show>
        </form>
        <p class="muted small">{st()?.remote_fix ? `Allowed: up to ${st()!.remote_fix!.max_runs_per_day} a day, $${st()!.remote_fix!.max_usd_per_run} each, from your own login.` : "Off: a fix request from a link waits for your click here."}</p>
        <form class="row small" data-testid="remote-send" onSubmit={(e) => { e.preventDefault(); void act(() => actions.ownerRequest("remote_send_set", { max_sends_per_day: Number(sends()) }), "Approvals from your phone allowed"); }}>
          <span class="muted" data-tip="“Approve” on a review link, from your own tailnet login, hands the export to Clippy's calendar without a click here; Clippy still asks before anything posts">Approve from the phone:</span>
          <input class="num" type="number" min="1" max="20" aria-label="Approvals per day" value={sends()} onInput={(e) => setSends(e.currentTarget.value)} />
          <span class="muted">a day</span>
          <button type="submit">Allow</button>
          <Show when={st()?.remote_send}>
            <button type="button" class="ghost" onClick={() => void act(() => actions.ownerRequest("remote_send_set", { off: true }), "Approvals from the phone off")}>Turn off</button>
          </Show>
          <Show when={st()?.remote_send}>{(r) => <span class="muted">on: {r().max_sends_per_day} a day</span>}</Show>
        </form>
      </details>
    </section>
  );
}
