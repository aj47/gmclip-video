// Images onto the frame (SPEC §5.9): paste (⌘V while a cut is open) or drop an image file
// on the viewer. The bytes go to the app once, which decodes them (R-MEDIA-SEC-3), keeps a
// PNG and puts the image on the cut at the playhead; the webview only ever gets thumbnails.
import { actions } from "../actions";
import { attempt, cutView, say, viewer } from "../store";

const IMAGE = /^image\/(png|jpeg)$/;
const MAX = 64 << 20;

async function b64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

/** Puts the image files of a paste or a drop on the cut; true when there were some. */
export async function addImages(files: FileList | File[] | null | undefined): Promise<boolean> {
  const v = cutView();
  const list = [...(files ?? [])].filter((f) => IMAGE.test(f.type));
  if (!v || !list.length) return false;
  for (const f of list) {
    if (f.size > MAX) {
      say(`${f.name} is over 64 MB`, "error");
      continue;
    }
    const data = await b64(f);
    await attempt(() => actions.imageOverlayAdd(v.project, v.cut, viewer()?.frame ?? 0, f.name || "pasted image", data));
  }
  return true;
}

/** ⌘V of an image while a cut is open (not into a text field). */
export function installImagePaste(): () => void {
  const onPaste = (e: ClipboardEvent) => {
    const t = e.target as HTMLElement | null;
    if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
    const files = e.clipboardData?.files;
    if (files && [...files].some((f) => IMAGE.test(f.type))) {
      e.preventDefault();
      void addImages(files);
    }
  };
  document.addEventListener("paste", onPaste);
  return () => document.removeEventListener("paste", onPaste);
}

/** Drag-and-drop handlers for the viewer box (the browser path; the app window delivers
 * dropped files as paths, handled in the store). */
export const imageDropHandlers = {
  onDragOver: (e: DragEvent) => {
    if ([...(e.dataTransfer?.items ?? [])].some((i) => i.kind === "file")) e.preventDefault();
  },
  onDrop: (e: DragEvent) => {
    if (![...(e.dataTransfer?.files ?? [])].some((f) => IMAGE.test(f.type))) return;
    e.preventDefault();
    void addImages(e.dataTransfer?.files);
  },
};
