// A direct core call in actions.ts whose arguments are not checked against the generated
// types (A2.01): no `satisfies Args<"cut_set">`.
import { call } from "../src/ipc";
export const actions = {
  cutRename: (project: string, cut: string, title: string) => call("cut_set", { cut, title }, project),
};
