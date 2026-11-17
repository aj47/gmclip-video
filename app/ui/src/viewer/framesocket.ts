// The frame socket client (SPEC D5, R-FRAME-1): one WebSocket per launch to the core's
// loopback port, token from `gm_call app_frame_socket` sent as the first message.
// Downstream: `GMF1` frames (32-byte header + NV12). Upstream: 32-byte input records.
import { actions } from "../actions";

export const RecordKind = { Down: 1, Move: 2, Up: 3, Wheel: 4, Key: 5, Viewport: 6, Cancel: 7 } as const;
export const Keys = { PlayPause: 1, StepBack: 2, StepForward: 3, Pause: 4 } as const;

export interface Frame {
  seq: number;
  outFrame: number;
  w: number;
  h: number;
  echoT: number;
  renderUs: number;
  flags: number;
  dropped: number;
  y: Uint8Array;
  uv: Uint8Array;
  receivedAt: number;
}

export interface RecordInit {
  buttons?: number;
  mods?: number;
  pointer?: number;
  x?: number;
  y?: number;
  dx?: number;
  dy?: number;
  code?: number;
  t?: number;
}

export function encodeRecord(kind: number, r: RecordInit): ArrayBuffer {
  const b = new ArrayBuffer(32);
  const v = new DataView(b);
  v.setUint8(0, kind);
  v.setUint8(1, r.buttons ?? 0);
  v.setUint8(2, r.mods ?? 0);
  v.setUint8(3, r.pointer ?? 0);
  v.setFloat32(4, r.x ?? 0, true);
  v.setFloat32(8, r.y ?? 0, true);
  v.setFloat32(12, r.dx ?? 0, true);
  v.setFloat32(16, r.dy ?? 0, true);
  v.setUint32(20, r.code ?? 0, true);
  v.setFloat64(24, r.t ?? performance.now(), true);
  return b;
}

function parse(buf: ArrayBuffer): Frame | null {
  if (buf.byteLength < 32) return null;
  const v = new DataView(buf);
  if (v.getUint32(0, true) !== 0x31464d47) return null;
  const w = v.getUint16(12, true);
  const h = v.getUint16(14, true);
  const ylen = w * h;
  const uvlen = (w >> 1) * (h >> 1) * 2;
  if (buf.byteLength !== 32 + ylen + uvlen) return null;
  return {
    seq: v.getUint32(4, true),
    outFrame: v.getUint32(8, true),
    w,
    h,
    echoT: v.getFloat64(16, true),
    renderUs: v.getUint32(24, true),
    flags: v.getUint16(28, true),
    dropped: v.getUint16(30, true),
    y: new Uint8Array(buf, 32, ylen),
    uv: new Uint8Array(buf, 32 + ylen, uvlen),
    receivedAt: performance.now(),
  };
}

function unhex(s: string): Uint8Array {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(2 * i, 2 * i + 2), 16);
  return out;
}

let shared: Promise<FrameSocket> | null = null;
const closedListeners = new Set<() => void>();
/** The token generation of the last connection (decision 0010: re-armed after a hide). */
let lastGeneration = 0;

export class FrameSocket {
  private listeners = new Set<(f: Frame) => void>();
  private constructor(private ws: WebSocket) {
    ws.onmessage = (m) => {
      const f = parse(m.data as ArrayBuffer);
      if (f) for (const l of this.listeners) l(f);
    };
  }

  /** The one connection this page makes (R-FRAME-1: the core accepts one per launch). */
  static get(): Promise<FrameSocket> {
    shared ??= (async () => {
      const { port, token, generation } = await actions.frameSocket();
      lastGeneration = generation ?? 0;
      const ws = new WebSocket(`ws://127.0.0.1:${port}/`);
      ws.binaryType = "arraybuffer";
      await new Promise<void>((resolve, reject) => {
        ws.onopen = () => resolve();
        ws.onerror = () => reject(new Error("the frame socket refused this page"));
      });
      ws.send(unhex(token));
      ws.onclose = () => {
        shared = null;
        closedListeners.forEach((f) => f());
      };
      return new FrameSocket(ws);
    })();
    // A failed attempt is not cached: the next mount may try again (the core still
    // accepts only one connection per launch).
    shared.catch(() => {
      shared = null;
    });
    return shared;
  }

  /** After a close: waits (polling once a second) until the core has re-armed the
   *  session with a new token, then connects again. Resolves with the new socket. */
  static async reconnect(stop: () => boolean): Promise<FrameSocket | null> {
    for (;;) {
      if (stop()) return null;
      await new Promise((r) => setTimeout(r, 1000));
      const info = await actions.frameSocket().catch(() => null);
      if (info && (info.generation ?? 0) > lastGeneration) {
        try {
          return await FrameSocket.get();
        } catch {
          // refused (raced or stale): keep waiting for the next generation
        }
      }
    }
  }

  /** Called when the socket closes (the viewer shows it is no longer live). */
  static onClose(cb: () => void): () => void {
    closedListeners.add(cb);
    return () => closedListeners.delete(cb);
  }

  onFrame(cb: (f: Frame) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  send(kind: number, r: RecordInit = {}) {
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(encodeRecord(kind, r));
  }

  get open(): boolean {
    return this.ws.readyState === WebSocket.OPEN;
  }
}
