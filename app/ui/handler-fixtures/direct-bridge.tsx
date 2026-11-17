// A handler that talks to the bridge itself instead of through actions.ts.
export function Bad() {
  return <button onClick={() => (globalThis as unknown as { __gmclipBridgeCall: (c: string) => void }).__gmclipBridgeCall("undo")}>x</button>;
}
