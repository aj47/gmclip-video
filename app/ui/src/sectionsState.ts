// Sections (decision 0046): the open cut's zoom and layout sections, read from
// `section_list` in cut ms, the one selected on the lanes (its controls in the
// inspector), whether the lanes are on, and a fixed zoom point being picked on the
// preview. The timeline's lanes read them again whenever the cut's rev moves, with the
// recording markers the cut plays (decision 0052), ticks in the ruler.
import { createSignal } from "solid-js";
import { recordingActions, studioActions, type CutMarker, type SectionView } from "./actions";
import { attempt, cutView, editing, sayUndo } from "./store";

export const [sections, setSections] = createSignal<{ cut: string; rev: number; list: SectionView[]; cutMs: number } | null>(null);
export const [selectedSection, setSelectedSection] = createSignal<string | null>(null);
/** The lanes turned on or off with the toolbar's toggle (null: shown on a recording or
 * once a section exists). Session-only. */
export const [sectionLanesOn, setSectionLanesOn] = createSignal<boolean | null>(null);
/** Whether the lanes show. */
export const sectionLanesShown = () => sectionLanesOn() ?? (hasRecording() || (cutSections()?.list.length ?? 0) > 0);
/** A zoom whose fixed point is being picked by a click on the preview. */
export const [pointPick, setPointPick] = createSignal<string | null>(null);
/** Whether the open project has a recorded screen (a source with a logged cursor). */
export const [hasRecording, setHasRecording] = createSignal(false);
/** The recording markers the cut plays, in cut ms. */
export const [recMarkers, setRecMarkers] = createSignal<{ cut: string; list: CutMarker[] } | null>(null);

let gen = 0;
/** Reads the cut's sections (and whether it has a recording, and its markers) again. */
export async function refreshSections() {
  const v = cutView();
  if (!v) {
    setRecMarkers(null);
    return setSections(null);
  }
  const g = ++gen;
  const [r, src, mk] = await Promise.all([
    studioActions.sectionList(v.project, v.cut).catch(() => null),
    recordingActions.sourceList(v.project).catch(() => null),
    studioActions.markerList(v.project, v.cut).catch(() => null),
  ]);
  if (g !== gen || cutView()?.cut !== v.cut) return;
  setHasRecording(Object.keys(src?.cursors ?? {}).length > 0);
  setRecMarkers({ cut: v.cut, list: mk?.markers ?? [] });
  setSections(r ? { cut: v.cut, rev: v.rev, list: r.sections, cutMs: r.cut_ms } : null);
  if (selectedSection() && !r?.sections.some((s) => s.id === selectedSection())) setSelectedSection(null);
}

/** The open cut's sections (none while another cut's are still shown). */
export const cutSections = () => {
  const s = sections();
  return s && s.cut === cutView()?.cut ? s : null;
};
/** The open cut's recording markers. */
export const cutMarkers = (): CutMarker[] => {
  const m = recMarkers();
  return m && m.cut === cutView()?.cut ? m.list : [];
};
export const sectionById = (id: string | null) => (id ? cutSections()?.list.find((s) => s.id === id) : undefined);

/** Removes a section (⌫ on a selected bar, or its controls' Remove). */
export async function removeSection(id: string) {
  const v = cutView();
  if (!v) return;
  const r = await editing(attempt(() => studioActions.sectionRemove(v.project, id)));
  if (selectedSection() === id) setSelectedSection(null);
  if (pointPick() === id) setPointPick(null);
  void refreshSections();
  if (r) sayUndo("Section removed", v.project);
}
