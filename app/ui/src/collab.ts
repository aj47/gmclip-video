// Decision 0015 (lane V): UI state shared by the comments panel, the viewer pins, the
// timeline markers and the thumbnail panel. No calls here (actions.ts has them).
import { createSignal } from "solid-js";
import type { CommentThread, FrameBox, ThumbStyle } from "./actions";

/** The open cut's threads, as the comments panel last read them. */
export const [threads, setThreads] = createSignal<CommentThread[]>([]);
/** Drawing a box on the frame for the next comment. */
export const [drawing, setDrawing] = createSignal(false);
export const [draftBox, setDraftBox] = createSignal<FrameBox | null>(null);
/** The thread picked in the panel (its pin stays shown while the playhead is elsewhere). */
export const [pickedThread, setPickedThread] = createSignal<string | null>(null);
/** A thumbnail title previewed on the frame (the thumbnail panel's options). */
export const [titlePreview, setTitlePreview] = createSignal<{ title: string; style: ThumbStyle; box: FrameBox; align: "left" | "centre" | "right" } | null>(null);

/** Timeline markers: threads still on the cut, by output frame. */
export const commentMarks = () =>
  threads()
    .filter((t) => t.frame !== null)
    .map((t) => ({ frame: t.frame as number, resolved: t.resolved, id: t.id }));
