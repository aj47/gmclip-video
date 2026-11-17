// What differs between macOS and Windows in the UI's words (M5 prep). The key handlers
// already take ⌘ or Ctrl; here the labels follow: ⌘ ⌥ ⇧ ⌫ on a Mac, Ctrl Alt Shift
// Backspace elsewhere. On a Mac every function returns its input unchanged.

export const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
export const isWindows = /Win/.test(navigator.platform || navigator.userAgent);

/** A `gm://` URL as this webview fetches it: WebView2 (Windows) has no custom schemes, so
 *  Tauri serves `gm://x` at `http://gm.localhost/x` there. Elsewhere unchanged. */
export function gmFetchUrl(url: string): string {
  return isWindows ? url.replace(/^gm:[/][/]/, "http://gm.localhost/") : url;
}

/** "this Mac" / "this PC". */
export const thisMachine = isMac ? "this Mac" : "this PC";
export const thisMachineTitle = isMac ? "This Mac" : "This PC";
/** The system's name ("Follow macOS" / "Follow Windows"). */
export const osName = isMac ? "macOS" : isWindows ? "Windows" : "the system";
/** The file manager ("Reveal in Finder" / "Reveal in File Explorer"). */
export const fileManager = isMac ? "Finder" : "File Explorer";

const NAMES: Record<string, string> = { "⌘": "Ctrl", "⌥": "Alt", "⇧": "Shift", "⌫": "Backspace" };

/** A key label or a sentence naming keys, for this platform: "⌘⇧Z" → "Ctrl+Shift+Z",
 *  "⌘-click" → "Ctrl-click", "(⇧ ten)" → "(Shift ten)". */
export function keyLabel(s: string): string {
  if (isMac) return s;
  return s.replace(/[⌘⌥⇧⌫]/g, (m, at: number) => {
    const next = s.charAt(at + 1);
    const joined = next !== "" && next !== " " && next !== "-" && next !== ")" && next !== "," && m !== "⌫";
    return NAMES[m] + (joined ? "+" : "");
  });
}

/** The file name of a path in either separator style. */
export function baseName(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

/** Is this the platform's "add to the selection" click modifier (⌘ on a Mac, Ctrl elsewhere;
 *  Ctrl-click is the context menu on a Mac)? */
export function isAddClick(e: { metaKey: boolean; ctrlKey: boolean }): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}
