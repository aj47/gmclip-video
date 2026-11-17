// Time left, said the way the import sheet and Home say it (SPEC §12.5's ETA chain:
// "Transcript in ~12 min · moments in ~14 min").
/** How long ago, said briefly ("just now", "12 min ago", "3 h ago", "2 d ago"). */
export function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 90) return "just now";
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  if (s < 129600) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} d ago`;
}

export function about(s: number | null | undefined): string {
  if (s === null || s === undefined || !Number.isFinite(s)) return "";
  if (s < 60) return "under a minute";
  const m = Math.round(s / 60);
  if (m < 60) return `~${m} min`;
  const h = Math.floor(m / 60);
  return `~${h} h ${String(m % 60).padStart(2, "0")} min`;
}
