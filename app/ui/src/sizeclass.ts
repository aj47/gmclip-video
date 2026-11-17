// Size classes (SPEC §12.2). Height is tested first.
import { createSignal } from "solid-js";

export type SizeClass = "phone" | "tablet" | "desktop" | "large";

export function classify(w: number, h: number): SizeClass {
  if (h < 700) return w < 600 ? "phone" : "tablet";
  if (w < 600) return "phone";
  if (w < 1100) return "tablet";
  if (w <= 1800) return "desktop";
  return "large";
}

const [size, setSize] = createSignal<SizeClass>(classify(innerWidth, innerHeight));
export { size };

export function watchSize() {
  const f = () => {
    const c = classify(innerWidth, innerHeight);
    setSize(c);
    document.documentElement.dataset.size = c;
  };
  f();
  addEventListener("resize", f);
}
