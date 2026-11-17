// The cut view (SPEC §12.2 Desktop): viewer, inspector, words strip, timeline; the
// header carries the title, export state and the Export button.
import { createEffect, createMemo, createResource, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { Portal } from "solid-js/web";
import { Banner, Button, Chip, IconButton, Menu, SegmentedControl, Tabs, type SegOption, type TabItem } from "../design/components";
import { Icon } from "../design/Icon";
import { ExportDoneChip, savedText, savedTip, topbarSlot } from "../components/Chrome";
import { MapSidebar } from "../components/MapSidebar";
import { setLaunchFor } from "../agentsState";
import { Splitter } from "../components/Splitter";
import { barTabs, layout, paneIsDrawer, paneShown, reading, setSideTab, setTightenOpen, showTab, SIDE_TABS, sideTab, tightenOpen, toggleFocus, togglePane, type SideTab } from "../layout";
import { prefs } from "../prefs";
import { Coach } from "../components/Coach";
import { actions, rate, type TermGate } from "../actions";
import { Inspector } from "../components/Inspector";
import { Story } from "../components/Story";
import { Timeline } from "../components/Timeline";
import { Transcript } from "../components/Transcript";
import { CropOverlay } from "../components/CropOverlay";
import { Gizmos } from "../components/Gizmos";
import { BrushOverlay } from "../components/BrushOverlay";
import { PointPicker } from "../components/SectionControls";
import { pointPick } from "../sectionsState";
import { brushOn } from "../roto";
import { LayoutPanel } from "../components/LayoutPanel";
import { ShortPanel } from "../components/ShortPanel";
import { TightenPanel } from "../components/TightenPanel";
import { PolishPanel } from "../components/PolishPanel";
import { AgentPanel } from "../components/AgentPanel";
import { ActivityRail } from "../components/ActivityRail";
import { Viewer } from "../components/Viewer";
import { BrollPop } from "../components/Broll";
import { WordsStrip } from "../components/WordsStrip";
import { LongformSheet } from "../components/LongformSheet";
import { ParentBanner } from "../components/ParentBanner";
import { ChaptersBar } from "../components/Packaging";
import { CutLikeSheet } from "../components/CutLikeSheet";
import { BothFramings } from "../components/FramingsSheet";
import { ExportFile, HandoffNote, lastExport } from "../components/ExportFile";
import { setFramingsFor } from "../extrasState";
import { cutLikeOpen, setCutLikeOpen } from "../libraryState";
import { CommentsPanel } from "../components/CommentsPanel";
import { CommentPins } from "../components/CommentPins";
import { ThumbPanel } from "../components/ThumbPanel";
import { longformOpen, projectMapData } from "../streamState";
import { askCrop, attempt, cropAsk, cutError, cutView, exportAsk, jobs, openCut, saved, say, selectedPart, setHelpOpen, setHistoryOpen, setSelectedPart, viewer } from "../store";

export function CutView() {
  const [starting, setStarting] = createSignal(false);
  const tab = sideTab;
  const setTab = setSideTab;
  // How the cut view lays out for this window (layout.ts, shell.css): wide ≥ 1280, medium
  // 900–1279, compact < 900 or short and narrow, portrait when taller than wide. Which
  // panes show is the View menu's (layout.ts): saved, and the same in every layout.
  const aspect = () => cutView()?.framing?.aspect ?? undefined;
  const sideShown = () => paneShown("side");
  let cutEl!: HTMLElement;
  const onOff = (b: boolean) => (b ? "on" : "off");
  const [cropping, setCroppingRaw] = createSignal(false);
  const setCropping = (on: boolean) => {
    const v = cutView();
    if (!v || on === cropping()) return;
    setCroppingRaw(on);
    // on: the effect below loads the focused layer's picture
    if (!on) void attempt(() => actions.viewerLoad(v.project, v.cut, { source: false, keep: true }));
  };
  // The part the layout tools act on: the selected one, else the one under the playhead.
  const part = createMemo(() => {
    const v = cutView();
    if (!v) return undefined;
    const sel = v.parts.find((p) => p.id === selectedPart());
    if (sel) return sel;
    const f = viewer()?.frame ?? 0;
    return v.parts.find((p) => f >= p.out[0] && f < p.out[1]) ?? v.parts[0];
  });
  // the layer in focus in crop mode (a click on its box or chip; else the role asked for,
  // else the first): the crop editor shows its own video
  const [cropFocus, setCropFocus] = createSignal<string | null>(null);
  const cropLayers = () => (part()?.layers ?? []).filter((l) => l.crop_n && l.model && l.visible);
  const cropLayer = createMemo(() => {
    const ls = cropLayers();
    return ls.find((l) => l.model === cropFocus()) ?? ls.find((l) => l.role === cropAsk()?.role) ?? ls[0];
  });
  // crop mode shows the focused layer's own picture (a recording's camera layer: the
  // camera file, not the screen; AJ 2026-10-20), and another part's when it changes
  const cropKey = createMemo(() => {
    const p = part();
    if (!cropping() || !p) return undefined;
    const l = cropLayer();
    return l?.model && l.source && l.source !== p.source ? `${p.id}:${l.model}` : p.id;
  });
  createEffect(on(cropKey, (k) => {
    const v = cutView();
    if (!k || !v) return;
    const [id, layer] = k.split(":");
    void attempt(() => actions.viewerLoad(v.project, v.cut, { source: true, keep: true, part: id, ...(layer ? { layer } : {}) }));
  }));
  // crop mode shows the picture at its own shape (a portrait screen recording is
  // portrait), not the 16:9 framing's
  const sourceAspect = () => {
    const s = cropLayer()?.src_size ?? part()?.layers.find((l) => l.src_size?.[0] && l.src_size?.[1])?.src_size;
    if (s && s[0] && s[1]) return s[0] / s[1];
    const l = cutView()?.framing?.list.find((x) => x.w > x.h);
    return l ? l.w / l.h : 16 / 9;
  };
  // a double-click on a layer (the viewer, or a layer's clip on the timeline): crop mode on
  // that part, the Look panel beside it; Esc leaves it
  createEffect(on(cropAsk, (a) => {
    if (!a) return;
    if (a.part) setSelectedPart(a.part);
    setCropFocus(null);
    showTab("look");
    setCropping(true);
  }, { defer: true }));
  createEffect(() => {
    if (!cropping()) return;
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || document.querySelector(".sheet")) return;
      e.preventDefault();
      setCropping(false);
    };
    addEventListener("keydown", esc);
    onCleanup(() => removeEventListener("keydown", esc));
  });
  const output = async (aspect: "9:16" | "16:9") => {
    const v = cutView();
    if (!v || v.framing?.aspect === aspect) return;
    if (aspect === "16:9") setCropping(false);
    await attempt(() => actions.outputSet(v.project, v.cut, aspect));
  };
  // The term gate before export (D.04/A.09): names the captions may spell wrong are shown;
  // the export goes ahead unchanged unless the human fixes them.
  const [gate, setGate] = createSignal<TermGate | null>(null);
  // the cut's checks (A2.17 lint + export staleness and exported siblings)
  const [checks] = createResource(
    () => { const v = cutView(); return v ? { p: v.project, c: v.cut, rev: v.rev, n: jobs().length } : undefined; },
    async (k) => (await attempt(() => actions.cutChecks(k.p, k.c))) ?? null,
  );
  // the export's format: the framing's own output, or a YouTube upload preset (16:9)
  const [exportPreset, setExportPreset] = createSignal("framed");
  const startExport = async (checked = false) => {
    const v = cutView();
    if (!v || starting()) return;
    setStarting(true);
    if (!checked && v.captions.some((c) => c.role === "caption")) {
      const g = await attempt(() => actions.termGate(v.project, v.cut));
      if (g && g.findings.length) {
        setGate(g);
        setStarting(false);
        return;
      }
    }
    setGate(null);
    const preset = v.upload_presets?.includes(exportPreset()) ? exportPreset() : undefined;
    await attempt(() => actions.exportStart(v.project, v.cut, false, preset));
    setStarting(false);
  };
  createEffect(on(exportAsk, () => void startExport(), { defer: true }));
  const fixAllAndExport = async () => {
    const v = cutView();
    const g = gate();
    if (!v || !g) return;
    for (const f of g.findings) await attempt(() => actions.captionReplace(v.project, v.cut, g.rev, [f.ordinals], f.written));
    await startExport(true);
  };
  // A2.17: a check's one-click fix ("Export again" is the export itself)
  const applyFix = async (rule: string, n: number, action?: string) => {
    const v = cutView();
    if (!v) return;
    if (action === "export") return void startExport();
    if (await attempt(() => actions.checkFix(v.project, v.cut, n, rule))) say("Fixed");
  };
  // The last export of this cut that failed (and nothing exported since): its step and a Retry.
  const [dismissed, setDismissed] = createSignal<string | null>(null);
  const failed = createMemo(() => {
    const v = cutView();
    if (!v) return undefined;
    const mine = jobs().filter((j) => j.cut === v.cut).sort((a, b) => b.started_ms - a.started_ms);
    const last = mine[0];
    return last && last.state === "failed" && last.id !== dismissed() ? last : undefined;
  });
  const loudnessLine = () => {
    const l = cutView()?.loudness;
    if (!l) return "";
    const plan = l.mode === "static" ? `${l.target_lufs} LUFS static, peaks ≤ ${l.true_peak} dBTP` : "as recorded";
    const m = l.last;
    const got = m && m.integrated !== null ? ` · last export ${m.integrated.toFixed(1)} LUFS, peak ${m.true_peak?.toFixed(1)} dBTP` : "";
    return `Loudness: ${plan}${got}`;
  };
  // H.06 (R-EXP-6): the last export's verify, and a re-verify of that file
  const [verifying, setVerifying] = createSignal(false);
  const verifyLine = () => {
    const e = cutView()?.export;
    const v = e?.verify;
    if (!e?.id) return "";
    if (!v) return "Last export: no verify recorded";
    const at = new Date(v.at_ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
    if (v.how === "export") return v.passed ? `Verified at export: ${v.summary}` : `Verify failed at export: ${v.summary}`;
    return v.passed ? `Re-verified at ${at}: ${v.summary}` : `Re-verify failed at ${at}: ${v.summary}`;
  };
  const reverify = async () => {
    const v = cutView();
    const id = v?.export.id;
    if (!v || !id) return;
    setVerifying(true);
    const r = await attempt(() => actions.exportVerify(v.project, id));
    setVerifying(false);
    if (r) say(r.passed ? `Export verified: ${r.summary}` : `Export failed verify: ${r.summary}`, r.passed ? "info" : "error");
  };
  const job = createMemo(() => {
    const v = cutView();
    return v ? jobs().find((j) => j.cut === v.cut && (j.state === "running" || j.state === "queued")) : undefined;
  });
  const exportChip = () => {
    const e = cutView()?.export;
    if (!e || e.state === "never") return "Not exported";
    const ago = Math.max(0, Math.round((Date.now() - (e.at_ms ?? 0)) / 60000));
    return e.state === "fresh" ? (ago < 1 ? "Exported just now" : `Exported ${ago} min ago`) : "Export out of date";
  };
  const exportState = () => {
    const e = cutView()?.export;
    if (!e || e.state === "never") return "Not exported yet";
    const ago = Math.max(0, Math.round((Date.now() - (e.at_ms ?? 0)) / 60000));
    return e.state === "fresh" ? `Exported ${ago} min ago` : `Export out of date — ${e.reason ?? "the cut changed"}`;
  };
  // the cut's own title, shown as a person would name it (an OBS file name reads as when
  // it was recorded); a click renames it
  const [renaming, setRenaming] = createSignal(false);
  const rename = (t: string) => {
    setRenaming(false);
    const v = cutView();
    const title = t.trim();
    if (v && title && title !== v.title) void attempt(() => actions.cutRename(v.project, v.cut, title));
  };
  const seconds = () => {
    const v = cutView();
    return v ? v.frames / rate(v.fps) : 0;
  };
  // "↶ N earlier versions" (SPEC §5.12): re-counted as the cut changes
  const [versionCount] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, rev: v.rev } : undefined;
    },
    async (k) => (await actions.historyCount(k.p).catch(() => null))?.total ?? 0,
  );
  const versions = () => versionCount() || 0;
  const meta = () => {
    const v = cutView();
    if (!v) return "";
    const n = v.parts.length;
    const raw = humanTitle(v.title) !== v.title ? `${v.title} · ` : "";
    return `${raw}${n} part${n === 1 ? "" : "s"} · ${clock(seconds())}`;
  };
  const framings = (): SegOption<"16:9" | "9:16">[] => [
    { value: "16:9", label: "16:9", icon: "landscape", hint: "Landscape (YouTube)" },
    { value: "9:16", label: "9:16", icon: "portrait", hint: "Vertical (Shorts, Reels, TikTok)" },
  ];
  const [exportOpts, setExportOptsRaw] = createSignal(false);
  let exportBox: HTMLSpanElement | undefined;
  // the popover is fixed under the button: the cut head (in the top bar) clips its own box
  const [popAt, setPopAt] = createSignal<{ top: number; right: number } | null>(null);
  const setExportOpts = (on: boolean) => {
    const r = exportBox?.getBoundingClientRect();
    setPopAt(on && r ? { top: Math.round(r.bottom + 6), right: Math.max(8, Math.round(innerWidth - r.right)) } : null);
    setExportOptsRaw(on);
  };
  onMount(() => {
    const away = (e: PointerEvent) => {
      if (exportOpts() && exportBox && !exportBox.contains(e.target as Node)) setExportOpts(false);
    };
    const resized = () => exportOpts() && setExportOpts(true);
    addEventListener("pointerdown", away, true);
    addEventListener("resize", resized);
    onCleanup(() => {
      removeEventListener("pointerdown", away, true);
      removeEventListener("resize", resized);
    });
  });
  // the newest export's file and the agent's handoff (the board folded in, decision 0054):
  // read again when the popover or the pill opens, and when an export or a run ends
  const [pillOpen, setPillOpen] = createSignal(false);
  const [tile] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, c: v.cut, e: v.export.id ?? "", w: v.agent_working?.identity ?? "", open: exportOpts() || pillOpen() } : undefined;
    },
    (k) => lastExport(k.p, k.c),
  );
  const loudChecks = () => (checks()?.findings ?? []).filter((f) => f.level !== "info");
  /** The status pill's tone: the worst of saving, the export and the checks. */
  const statusTone = (): "neutral" | "success" | "warning" | "danger" => {
    const e = cutView()?.export.state;
    if (saved() === "failed" || loudChecks().some((f) => f.level === "error")) return "danger";
    if (loudChecks().length || e === "stale") return "warning";
    return e === "fresh" && saved() === "saved" ? "success" : "neutral";
  };
  const tabs = (): TabItem<SideTab>[] => barTabs().map((id) => SIDE_TABS.find((t) => t.id === id)!);
  return (
    <main
      class="cut"
      aria-label="Cut"
      ref={cutEl}
      data-layout={layout()}
      data-aspect={aspect()}
      data-side={onOff(sideShown())}
      data-side-drawer={paneIsDrawer("side") ? "1" : undefined}
      data-tl={onOff(paneShown("timeline"))}
      data-words={onOff(paneShown("words") && !cropping())}
      data-cropping={cropping() ? "on" : undefined}
      data-focus={prefs.focus && !reading() ? "on" : undefined}
      data-reading={reading() ? "on" : undefined}
      data-side-w={prefs.side_w ? "1" : undefined}
      data-map-w={prefs.map_w ? "1" : undefined}
      data-tl-h={prefs.tl_h ? "1" : undefined}
      style={{ "--side-w": `${prefs.side_w || 0}px`, "--map-w": `${prefs.map_w || 0}px`, "--tl-h": `${prefs.tl_h || 0}px` }}
    >
      <Show when={topbarSlot()}>
        {(slot) => (
          <Portal mount={slot()}>
            <header class="cut-head" data-layout={layout()}>
              <div class="cut-titles">
                <Show
                  when={!renaming()}
                  fallback={
                    <input
                      class="cut-title-input"
                      aria-label="Cut title"
                      value={cutView()?.title ?? ""}
                      ref={(el) => queueMicrotask(() => el.select())}
                      onKeyDown={(e) => {
                        e.stopPropagation();
                        if (e.key === "Enter") rename(e.currentTarget.value);
                        if (e.key === "Escape") setRenaming(false);
                      }}
                      onBlur={(e) => rename(e.currentTarget.value)}
                    />
                  }
                >
                  <h1 data-testid="cut-title" data-tip={cutView() ? "Rename" : undefined} onClick={() => cutView() && setRenaming(true)}>
                    {cutView() ? cutView()!.title : cutError() ? "This cut cannot be shown" : "Loading…"}
                  </h1>
                </Show>
                <div class="cut-subline">
                <span class="cut-sub" data-testid="cut-meta" data-parts={cutView()?.parts.length ?? 0} data-seconds={seconds().toFixed(2)}>
                  {meta()}
                </span>
                <Show when={versions()}>
                  {(n) => (
                    <button type="button" class="versions-link" data-testid="earlier-versions" data-tip="History: compare and restore" onClick={() => setHistoryOpen({})}>
                      ↶ {n()} earlier version{n() === 1 ? "" : "s"}
                    </button>
                  )}
                </Show>
                </div>
              </div>
              <div class="cut-center">
                <SegmentedControl label="Output" options={framings()} value={cutView()?.framing?.aspect === "9:16" ? "9:16" : cutView()?.framing?.aspect === "16:9" ? "16:9" : undefined} onChange={(a) => void output(a)} />
              </div>
              <div class="cut-right">
                <ExportDoneChip />
                <Show when={cutView()?.agent_working}>
                  {(w) => (
                    <Chip tone="warning" dot role="status" testid="agent-working" tooltip={`${w().identity} has a run open on this cut: its edits land here live. Undo run in History takes them back.`}>
                      Agent working on this cut
                    </Chip>
                  )}
                </Show>
                {/* one status pill: saved, exported, the cut's checks (AJ 2026-10-18) */}
                <details class="checks status-pill" data-testid="checks" onToggle={(e) => setPillOpen(e.currentTarget.open)}>
                  <summary class={`chip chip-${statusTone()}`}>
                    <Icon name={statusTone() === "danger" || statusTone() === "warning" ? "triangle-alert" : "circle-check"} size={12} />
                    <span class={`saved${saved() === "failed" ? " failed" : ""}`} data-testid="saved" role={saved() === "failed" ? "alert" : undefined}>{savedText()}</span>
                    <span class="status-sep" aria-hidden="true">·</span>
                    <span class={`export-state ${cutView()?.export.state ?? ""}`}>{exportChip()}</span>
                    <Show when={loudChecks().length}>
                      {(n) => (
                        <>
                          <span class="status-sep" aria-hidden="true">·</span>
                          <span>{n()} check{n() === 1 ? "" : "s"}</span>
                        </>
                      )}
                    </Show>
                    <Show when={tile()?.handoff}>
                      <span class="status-sep" aria-hidden="true">·</span>
                      <span data-testid="handoff-chip">Agent's note</span>
                    </Show>
                  </summary>
                  <ul>
                    <li class="small status-line">{savedTip() ?? savedText()}</li>
                    <li class="small status-line">{exportState()}</li>
                    <Show when={cutView() && tile()?.handoff ? tile() : null}>
                      {(t) => <li><HandoffNote project={cutView()!.project} tile={t()} /></li>}
                    </Show>
                    <For each={checks()?.findings ?? []} fallback={<li class="muted small">No checks to look at.</li>}>
                      {(f, i) => (
                        <li class={`small ${f.level}`} data-rule={f.rule}>
                          <b>{f.level}</b> {f.message}
                          <Show when={f.fix}>
                            {(fx) => (
                              <>
                                {" "}
                                <button type="button" class="ghost small" data-testid="check-fix" onClick={() => void applyFix(f.rule, i(), fx().action)}>
                                  {fx().label}
                                </button>
                              </>
                            )}
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </details>
                <span class="tb-divider" />
                <Menu
                  label="More"
                  icon="more"
                  items={() => [
                    ...(prefs.developer ? [{ label: "Ask an agent…", icon: "smile", onSelect: () => { const v = cutView(); if (v) setLaunchFor({ project: v.project, cut: v.cut }); } }] : []),
                    { label: "History", icon: "history", keys: "⌘⇧S", testid: "history-open", onSelect: () => setHistoryOpen({}) },
                    { label: "New cut like this…", icon: "layers", testid: "cut-like-open", onSelect: () => setCutLikeOpen(true) },
                    { label: "Keyboard shortcuts", icon: "keyboard", keys: "?", onSelect: () => setHelpOpen(true) },
                  ]}
                />
                <IconButton icon="panel-right" class="side-toggle" label={sideShown() ? "Hide the side panel" : "Show the side panel"} keys="⌥2" pressed={sideShown()} onClick={() => togglePane("side")} />
                <Show
                  when={job()}
                  fallback={
                    <span class="export-split" ref={exportBox}>
                      <Button variant="primary" icon="share" data-testid="export" keys="⌘E" tooltip="Export the cut" disabled={!cutView() || starting()} onClick={() => void startExport()}>
                        Export
                      </Button>
                      <button type="button" class="primary export-more" aria-label="Export options" aria-expanded={exportOpts()} onClick={() => setExportOpts(!exportOpts())}>
                        <Icon name="chevron-down" size={13} />
                      </button>
                      <div
                        classList={{ popover: true, fixed: !!popAt() }}
                        style={popAt() ? { top: `${popAt()!.top}px`, right: `${popAt()!.right}px`, "--pop-top": `${popAt()!.top}px` } : undefined}
                        hidden={!exportOpts()}
                        role="group"
                        aria-label="Export options"
                      >
                        <h3>Export options</h3>
                        <label class="field">
                          Loudness
                          <select
                            aria-label="Export loudness"
                            value={cutView()?.loudness?.mode === "static" ? String(cutView()?.loudness?.target_lufs) : "as_recorded"}
                            onChange={(e) => {
                              const v = cutView();
                              const x = e.currentTarget.value;
                              if (v) void attempt(() => actions.loudnessSet(v.project, v.cut, x === "as_recorded" ? null : Number(x)));
                            }}
                          >
                            <option value="as_recorded">As recorded</option>
                            <option value="-16">−16 LUFS (Reels, Shorts)</option>
                            <option value="-14">−14 LUFS (YouTube)</option>
                          </select>
                        </label>
                        <span class="hint" data-testid="loudness">{loudnessLine()}</span>
                        <Show when={cutView()}>
                          {(v) => <BothFramings project={v().project} cut={v().cut} rev={v().rev} open={exportOpts()} onSeveral={() => { setExportOpts(false); setFramingsFor({ project: v().project, only: v().cut }); }} />}
                        </Show>
                        <Show when={cutView()?.export.id}>
                          <div class="export-verify" data-testid="export-verify" data-passed={cutView()?.export.verify ? String(cutView()!.export.verify!.passed) : "none"}>
                            <span class="hint" data-testid="export-verify-line">{verifyLine()}</span>
                            <Button size="sm" variant="ghost" icon="check" data-testid="export-reverify" disabled={verifying()} tooltip="Read the last export again: the file, its SHA-256 against its record, and its container against the cut's plan" onClick={() => void reverify()}>
                              {verifying() ? "Verifying…" : "Re-verify"}
                            </Button>
                            <Show when={(cutView()?.export.verify?.checks.length ?? 0) + (cutView()?.export.verify?.not_run.length ?? 0)}>
                              <details class="small">
                                <summary>Checks</summary>
                                <ul class="verify-checks">
                                  <For each={cutView()?.export.verify?.checks ?? []}>
                                    {(c) => <li data-ok={String(c.ok)}>{c.ok ? "✓" : "✗"} {c.step}: {c.detail}</li>}
                                  </For>
                                  <For each={cutView()?.export.verify?.not_run ?? []}>{(n) => <li class="muted">not run: {n}</li>}</For>
                                </ul>
                              </details>
                            </Show>
                          </div>
                        </Show>
                        <Show when={cutView() && tile() ? tile() : null}>
                          {(t) => <ExportFile project={cutView()!.project} tile={t()} />}
                        </Show>
                        <Show when={(cutView()?.upload_presets ?? []).length}>
                          <label class="field">
                            Format
                            <select aria-label="Export format" data-testid="export-preset" value={exportPreset()} onChange={(e) => setExportPreset(e.currentTarget.value)}>
                              <option value="framed">As framed ({cutView()?.canvas.w}×{cutView()?.canvas.h})</option>
                              <For each={cutView()?.upload_presets ?? []}>
                                {(p) => <option value={p}>{p === "youtube-1440p" ? "YouTube 1440p (2560×1440)" : "YouTube 1080p (1920×1080)"}</option>}
                              </For>
                            </select>
                          </label>
                        </Show>
                      </div>
                    </span>
                  }
                >
                  {(j) => (
                    <span class="exporting" data-testid="export-progress">
                      {j().phase === "captions" ? "Hearing captions" : j().phase === "audio" ? "Mixing audio" : "Exporting"} <progress max={j().frames} value={j().done} /> {Math.round((100 * j().done) / Math.max(1, j().frames))}%
                      <IconButton icon="x" label="Cancel" size="sm" onClick={() => void attempt(() => actions.exportCancel(j().id))} />
                    </span>
                  )}
                </Show>
              </div>
            </header>
          </Portal>
        )}
      </Show>
      <div class="cut-alerts">
        <Show when={cutError()}>
          {(e) => {
            const x = explainPlanError(e());
            return (
              <Banner tone="danger" testid="cut-error" role="alert">
                <span class="plan-error">
                  <b>{x.part ? `Part ${x.part}: ` : ""}{x.what}</b>
                  <span>{x.fix}</span>
                  <span class="muted small">{cutView() ? "The viewer keeps the last frame that planned." : ""}</span>
                </span>
              </Banner>
            );
          }}
        </Show>
        <Show when={failed()}>
          {(j) => (
            <Banner tone="danger" testid="export-failed" onDismiss={() => setDismissed(j().id)} actions={<button type="button" class="small" onClick={() => { setDismissed(j().id); void startExport(); }}>Retry</button>}>
              <span class="export-failed">Export failed — {j().error ?? "unknown"}</span>
            </Banner>
          )}
        </Show>
        <Show when={gate()}>
          {(g) => (
            <Banner
              tone="warning"
              testid="term-gate"
              role="alert"
              onDismiss={() => setGate(null)}
              actions={
                <>
                  <button type="button" class="primary small" data-testid="term-gate-fix" onClick={() => void fixAllAndExport()}>Fix and export</button>
                  <button type="button" class="ghost small" data-testid="term-gate-ignore" onClick={() => void startExport(true)}>Export as is</button>
                </>
              }
            >
              {g().findings.length} name{g().findings.length === 1 ? "" : "s"} may be spelt wrong in the captions:{" "}
              {g().findings.slice(0, 4).map((f) => `${f.shown} → ${f.written}`).join(", ")}{g().findings.length > 4 ? "…" : ""}
            </Banner>
          )}
        </Show>
        <ParentBanner />
        <ChaptersBar />
      </div>
      {/* the map's way back when hidden is the top bar's toggle (and ⌥1) */}
      <MapSidebar />
      <Show when={prefs.focus && !reading()}>
        <button type="button" class="focus-exit" data-testid="focus-exit" data-tip="Every pane back" data-kbd="Esc" onClick={() => toggleFocus()}>
          <Icon name="focus" size={13} />Focus · show panes
        </button>
      </Show>
      <Coach id="welcome">
        Hover the timeline to scrub (<kbd>⌥S</kbd> turns it off). Drag a part's edge to trim, drag a part to move it, click a word to jump there. Every key is under <kbd>?</kbd>
      </Coach>
      {/* crop mode: the way out, over the viewer */}
      <Show when={cropping()}>
        <button type="button" class="focus-exit crop-exit" data-testid="crop-exit" data-tip="Back to the frame" data-kbd="Esc" onClick={() => setCropping(false)}>
          <Icon name="check" size={13} />Done cropping
        </button>
      </Show>
      <Viewer
        onDblClick={() => askCrop(null)}
        aspect={() => (cropping() ? sourceAspect() : undefined)}
        overlay={pointPick() ? (r) => <PointPicker rect={r} /> : cropping() ? (r) => <CropOverlay rect={r} part={part()} focus={cropLayer()} setFocus={setCropFocus} /> : brushOn() ? (r) => <BrushOverlay rect={r} part={part()} /> : (r) => <><Gizmos rect={r} /><CommentPins rect={r} /></>}
      />
      <Show when={paneShown("map") && layout() === "wide"}>
        <Splitter
          pref="map_w"
          what="project map"
          class="split-map"
          axis="x"
          dir={1}
          testid="split-map"
          measure={() => cutEl.querySelector(".map")?.getBoundingClientRect().width ?? 232}
          range={() => [160, Math.min(480, cutEl.clientWidth - 520)]}
        />
      </Show>
      <Show when={sideShown() && !paneIsDrawer("side") && layout() !== "portrait"}>
        <Splitter
          pref="side_w"
          what="side panel"
          class="split-side"
          axis="x"
          dir={-1}
          testid="split-side"
          measure={() => cutEl.querySelector(".side")?.getBoundingClientRect().width ?? 360}
          range={() => [240, Math.max(240, cutEl.clientWidth - (cutEl.querySelector(".map")?.getBoundingClientRect().width ?? 0) - 300)]}
        />
      </Show>
      <aside class="side" aria-label="Side panel" data-open={String(sideShown())} data-tab={tab()}>
        <div class="side-tabs">
          <Tabs label="Side panel" items={tabs()} value={tab()} onChange={setTab} />
        </div>
        {/* Words: Tighten folded above the transcript. The transcript stays mounted: the
            global keys (d/x, s, p, q/w) read it. */}
        <div class="side-pane words-pane" hidden={tab() !== "words"}>
          <details class="words-tighten" data-testid="words-tighten" open={tightenOpen()} onToggle={(e) => setTightenOpen(e.currentTarget.open)}>
            <summary><Icon name="tighten" size={13} />Tighten: pauses and fillers</summary>
            <Show when={tightenOpen()}><TightenPanel /></Show>
          </details>
          <Transcript />
        </div>
        <Switch>
          {/* Look: the layout (crops, insets, presets), then the part's inspector, one pane */}
          <Match when={tab() === "look"}>
            <div class="side-stack" data-testid="look-pane">
              <LayoutPanel part={part()} cropping={cropping()} setCropping={setCropping} />
              <Inspector />
            </div>
          </Match>
          <Match when={tab() === "captions"}><ShortPanel /></Match>
          <Match when={tab() === "story"}><Story /></Match>
          <Match when={tab() === "polish"}><PolishPanel /></Match>
          <Match when={tab() === "agent"}><AgentPanel /><ActivityRail /></Match>
          <Match when={tab() === "review"}><CommentsPanel /></Match>
          <Match when={tab() === "thumbnail"}><ThumbPanel /></Match>
        </Switch>
      </aside>
      <WordsStrip />
      <BrollPop />
      <Show when={paneShown("timeline") && !reading()}>
        <Splitter
          pref="tl_h"
          what="timeline"
          class="split-tl"
          axis="y"
          dir={-1}
          testid="split-tl"
          measure={() => cutEl.querySelector(".timeline")?.getBoundingClientRect().height ?? 220}
          range={() => [96, Math.max(96, cutEl.clientHeight - 150)]}
        />
      </Show>
      <Timeline />
      <Show when={longformOpen()}>
        <LongformSheet />
      </Show>
      <Show when={cutLikeOpen() && cutView()}>
        {(v) => <CutLikeSheet project={v().project} cut={v().cut} title={v().title} onClose={() => setCutLikeOpen(false)} />}
      </Show>
    </main>
  );
}

/** "m:ss" (or "h:mm:ss") for a length in seconds. */
export function clock(s: number): string {
  const t = Math.round(s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const ss = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** A recording's file name as a person would say it: OBS's "2026-05-07 13-40-05" reads
 * "Recording, 7 May 13:40"; anything else is itself. */
export function humanTitle(t: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})[ _](\d{2})-(\d{2})(?:-(\d{2}))?$/.exec(t.trim());
  if (!m) return t;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), Number(m[4]), Number(m[5]));
  return `Recording, ${d.toLocaleDateString(undefined, { day: "numeric", month: "short" })} ${m[4]}:${m[5]}`;
}

/** A planner error in words: what failed, which part, and the fix when it is known. */
export function explainPlanError(msg: string): { part: number | null; what: string; fix: string } {
  const m = /part (\d+): (.*)$/.exec(msg);
  const what = (m ? m[2]! : msg.replace(/^the cut does not plan yet: (the planned RenderPlan fails validation: )?/, "")).trim();
  const part = m ? Number(m[1]) : null;
  const fixes: [RegExp, string][] = [
    [/off the frame grid/, "Nudge that edge by a frame (‹ ›) or undo the last change (⌘Z)."],
    [/(hide|content hold) that starts or ends inside a part/, "Split the part at the span's edges, or remove the span (Look › Sound and camera)."],
    [/cut's frame rate/, "Open the video on its own (Home › Open any video): its cut takes the video's frame rate."],
    [/own nominal rate|frame rate/, "Open the video on its own (Home › Open any video) so the cut takes its frame rate, or undo (⌘Z)."],
    [/only 48 kHz sound/, "Export the video's sound at 48 kHz (most recorders have the setting), or pair a 48 kHz sound file."],
    [/speed change/, "Set the part back to normal speed."],
    [/no picture member|no audio member/, "Pair the files again (Home › Open a stream with its hi-res picture…)."],
    [/does not resolve|media this cut does not play/, "Something it is anchored to is gone: undo (⌘Z), or remove that span."],
    [/no parts/, "Add a part from the transcript (select words › ＋ Add as part)."],
  ];
  const fix = fixes.find(([re]) => re.test(what))?.[1] ?? "⌘Z undoes the last change.";
  return { part, what, fix };
}
