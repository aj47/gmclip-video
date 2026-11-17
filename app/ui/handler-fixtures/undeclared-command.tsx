// A handler calling a command nobody declares.
import { call } from "../src/ipc";
export function Bad() {
  return <button onClick={() => void call("owner_secret_grant", {})}>x</button>;
}
