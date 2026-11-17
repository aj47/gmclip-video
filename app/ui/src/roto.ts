// The edge brush's state (R.02): on/off, add or subtract, size (a fraction of the source
// height) and feather. The brush overlay paints on the viewer; the roto panel sets these.
import { createSignal } from "solid-js";

export const [brushOn, setBrushOn] = createSignal(false);
export const [brushMode, setBrushMode] = createSignal<"add" | "subtract">("subtract");
export const [brushSize, setBrushSize] = createSignal(0.02);
export const [brushFeather, setBrushFeather] = createSignal(0.5);
