// Lane L's shared UI state: the "New cut like this" sheet (opened from the cut view's
// toolbar, its narrow-window menu, or ⌘K).
import { createSignal } from "solid-js";

export const [cutLikeOpen, setCutLikeOpen] = createSignal(false);
