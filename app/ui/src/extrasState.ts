// Sheets of the recommended extras (lane K5), opened from several places (the export
// popover, Home's project cards, ⌘K).
import { createSignal } from "solid-js";

/** "Export 9:16 and 16:9" for a project's cuts (`only`: the cut ticked when it opens). */
export const [framingsFor, setFramingsFor] = createSignal<{ project: string; only?: string } | null>(null);
