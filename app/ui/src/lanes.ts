// Overlay lanes (SPEC §5.3): greedy packing — overlays in order of start, each into the
// first lane whose last overlay has ended, a new lane when none has. No model math: the
// frames are the plan's own.

export interface Spanned {
  out: [number, number];
}

/** The lane of each item (same order as given) and how many lanes there are. */
export function pack<T extends Spanned>(items: T[]): { lane: number[]; lanes: number } {
  const order = items.map((_, i) => i).sort((a, b) => items[a]!.out[0] - items[b]!.out[0] || items[a]!.out[1] - items[b]!.out[1]);
  const ends: number[] = [];
  const lane = new Array<number>(items.length).fill(0);
  for (const i of order) {
    const [a, b] = items[i]!.out;
    let k = ends.findIndex((e) => e <= a);
    if (k < 0) {
      k = ends.length;
      ends.push(b);
    } else ends[k] = b;
    lane[i] = k;
  }
  return { lane, lanes: ends.length };
}
