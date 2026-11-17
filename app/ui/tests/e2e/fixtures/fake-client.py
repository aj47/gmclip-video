#!/usr/bin/python3
# A scripted stand-in for the four agent clients the launcher runs (Claude Code, Codex, pi,
# opencode), for the QA agent workflow (qa-w3-agent.spec.ts) and the find-moments pass
# (findmoments.spec.ts). No model: it reads the
# per-run MCP config the way its client would (claude --mcp-config, codex -c
# mcp_servers.gmclip.*, pi's -e extension, opencode's OPENCODE_CONFIG), starts gmclip's MCP
# shim with the environment the launcher gave it, makes a short scripted list of calls on
# the run's cut, and prints the transcript in that client's own JSON shape. The launcher
# runs only native clients, so global-setup.ts compiles a tiny exec wrapper per client
# (`claude`, `codex`, `pi`, `opencode`) that runs `python3 <this file> <name> <args…>`.
import json, os, re, subprocess, sys, time

me = sys.argv[1]
VERSIONS = {"claude": "2.1.300 (Claude Code)", "codex": "codex-cli 0.161.0", "pi": "0.70.2", "opencode": "1.2.3"}
argv = sys.argv[2:]
if argv[:1] == ["--version"] or argv[:1] == ["-v"]:
    print(VERSIONS[me])
    sys.exit(0)

def after(flag):
    return argv[argv.index(flag) + 1] if flag in argv else None

if me == "claude":
    cfg = json.load(open(after("--mcp-config")))["mcpServers"]["gmclip"]
    cmd = [cfg["command"]] + cfg["args"]
elif me == "codex":
    kv = dict(argv[i + 1].split("=", 1) for i, a in enumerate(argv) if a == "-c" and "=" in argv[i + 1])
    cmd = [json.loads(kv["mcp_servers.gmclip.command"])] + json.loads(kv["mcp_servers.gmclip.args"])
elif me == "pi":
    first = open(after("-e")).readline()
    g = json.loads(re.match(r"const GMCLIP = (.*);\s*$", first).group(1))
    cmd = [g["command"]] + g["args"]
else:
    c = json.load(open(os.environ["OPENCODE_CONFIG"]))["mcp"]["gmclip"]["command"]
    cmd = list(c)

brief = sys.stdin.read()
cut = brief.split("work on cut ")[1].split(" ")[0].rstrip(".,")
p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=os.environ)
n = [0]

def rpc(method, params=None, notify=False):
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    if not notify:
        n[0] += 1
        msg["id"] = n[0]
    p.stdin.write((json.dumps(msg) + "\n").encode())
    p.stdin.flush()
    if notify:
        return None
    while True:
        r = json.loads(p.stdout.readline())
        if r.get("id") == n[0]:
            return r

def out(o):
    print(json.dumps(o), flush=True)

sid = f"ses_{me}_{os.getpid()}"
# the session files pi and opencode keep in the run folder (what a continue resumes)
if me == "pi":
    d = after("--session-dir")
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, f"{sid}.jsonl"), "w") as f:
        f.write(json.dumps({"type": "session", "version": 3, "id": sid}) + "\n")
elif me == "opencode":
    with open(os.path.join(os.path.dirname(os.environ["OPENCODE_CONFIG"]), "opencode.db"), "wb") as f:
        f.write(b"fake session store\n")
if me == "pi":
    out({"type": "session", "version": 3, "id": sid, "timestamp": "t", "cwd": os.getcwd()})
    out({"type": "agent_start"})
rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": me, "version": VERSIONS[me]}})
rpc("notifications/initialized", notify=True)
names = ["mcp__gmclip__" + t["name"] for t in rpc("tools/list", {})["result"]["tools"]]
if me == "claude":
    out({"type": "system", "subtype": "init", "tools": names})

def call(i, tool, args):
    if me == "codex":
        out({"type": "item.started", "item": {"id": f"i{i}", "type": "mcp_tool_call", "server": "gmclip", "tool": tool, "arguments": args, "status": "in_progress"}})
    elif me == "pi":
        out({"type": "tool_execution_start", "toolCallId": f"c{i}", "toolName": "mcp__gmclip__" + tool, "args": args})
    elif me == "claude":
        out({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": f"t{i}", "name": "mcp__gmclip__" + tool, "input": args}]}})
    r = rpc("tools/call", {"name": tool, "arguments": args})
    res = r.get("result", {})
    text = "".join(c.get("text", "") for c in res.get("content", []))
    err = bool(res.get("isError")) or "error" in r
    if me == "codex":
        out({"type": "item.completed", "item": {"id": f"i{i}", "type": "mcp_tool_call", "server": "gmclip", "tool": tool, "arguments": args, "status": "failed" if err else "completed"}})
    elif me == "pi":
        out({"type": "tool_execution_end", "toolCallId": f"c{i}", "toolName": "mcp__gmclip__" + tool, "result": {}, "isError": err})
    elif me == "claude":
        out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": f"t{i}", "is_error": err, "content": text[:200]}]}})
    else:
        out({"type": "tool_use", "timestamp": i, "sessionID": sid, "part": {"type": "tool", "tool": "gmclip_" + tool, "callID": f"x{i}", "state": {"status": "error" if err else "completed", "input": args}}})
    if err:
        return {"_text": text, "_error": True}
    try:
        return json.loads(text)
    except Exception:
        return {"_text": text, "_error": err}

def finish(summary):
    p.stdin.close()
    p.wait(timeout=20)
    if me == "claude":
        out({"type": "result", "result": summary, "usage": {"input_tokens": 0, "output_tokens": 0}})
    elif me == "codex":
        out({"type": "item.completed", "item": {"id": "i9", "type": "agent_message", "text": summary}})
        out({"type": "turn.completed", "usage": {"input_tokens": 0, "output_tokens": 0}})
    elif me == "pi":
        out({"type": "message_end", "message": {"role": "assistant", "content": [{"type": "text", "text": summary}], "usage": {"input": 0, "output": 0}}})
        out({"type": "agent_end", "messages": []})
    else:
        out({"type": "text", "timestamp": 8, "sessionID": sid, "part": {"type": "text", "text": summary}})
        out({"type": "step_finish", "timestamp": 9, "sessionID": sid, "part": {"type": "step-finish", "reason": "stop", "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}}}})
    sys.exit(0)

# The moment passes (A.01) speak the pass contract: begin, then answer each step from its
# own served lines — the first read cards the first line it is shown, every other step
# submits nothing — until gmclip says done.
for pass_name in ("find_moments", "full_sweep"):
    if f'pass: "{pass_name}"' in brief:
        step = call(1, "ai_pass", {"op": "begin_self", "pass": pass_name, "target": {}})
        i = 2
        while not step.get("done") and i < 60:
            items = []
            if step.get("step", {}).get("kind") == "first" and step.get("lines"):
                line = step["lines"][0]
                ref = line.split(" ", 1)[0]
                text = line.split("<untrusted>", 1)[1].rsplit("</untrusted>", 1)[0]
                items = [{"start_line": ref, "end_line": ref, "quote": text, "score": 4, "type": "result", "why": "the opening line (scripted)"}]
            step = call(i, "pass_step", {"pass_run": step["pass_run"], "submit": items})
            i += 1
        rec = step.get("record") or {}
        verdict = (rec.get("density") or {}).get("verdict", "no verdict")
        finish(f"{me}: {pass_name} wrote {(rec.get('stats') or {}).get('written', 0)} moment(s); {verdict}")

# Decision 0024: the title/description pass speaks its contract the same way: begin, then
# one draft per platform it is missing, written from the cut's own first line (so every
# number in it is said in the cut), until gmclip says done.
if 'pass: "publish_copy"' in brief:
    step = call(1, "ai_pass", {"op": "begin_self", "pass": "publish_copy", "target": {}})
    first = ""
    if step.get("lines"):
        first = step["lines"][0].split("<untrusted>", 1)[1].rsplit("</untrusted>", 1)[0]
    title = " ".join(first.split()[:8]).rstrip(",.") or "A clip"
    i = 2
    written = 0
    while not step.get("done") and i < 8:
        items = [{"platform": p, "title": f"{title} ({p})"[:90], "description": f"{first}\n#clip"} for p in step.get("missing", [])]
        step = call(i, "pass_step", {"pass_run": step["pass_run"], "submit": items})
        written += (step.get("submitted") or {}).get("accepted", 0)
        i += 1
    finish(f"{me}: publish_copy proposed {written} draft(s) for you to accept, edit or reject")

got = call(1, "cut_get", {"cut": cut})
as_of = got.get("as_of")
# what it reads (the run's "looking at")
call(2, "transcript_read", {"source": (got.get("sources") or [{}])[0].get("id", "") if isinstance(got.get("sources"), list) else "", "from_s": 0, "to_s": 60})
time.sleep(float(os.environ.get("GMCLIP_FAKE_DWELL", "2.5")))
note = "continued" if "--session" in argv or "previous run" in brief.lower() or "continue" in brief.lower() else "first"
r = call(3, "cut_edit", {"cut": cut, "base": {"as_of": as_of}, "ops": [{"op": "cut_set", "title": f"{me} pass ({note})"}]})
summary = f"{me}: retitled the cut ({note} run)" + ("" if not r.get("_error") else f"; the edit was refused: {r.get('_text', '')[:120]}")
# E.01 (batchcreds.spec): a note "probe-cut:<id>" on the run's own cut names another cut;
# the run tries to read and edit it, and says what gmclip answered, and which scopes its
# tools cover (a launched run's credential is bound to its own cut)
probe = re.search(r"probe-cut:([0-9A-HJKMNP-TV-Z]{26})", json.dumps(got))
if probe:
    other = probe.group(1)
    o = call(4, "cut_get", {"cut": other})
    e = call(5, "cut_edit", {"cut": other, "base": {"as_of": as_of}, "ops": [{"op": "cut_set", "title": "taken over"}]})
    def said(x):
        if not x.get("_error"):
            return "allowed"
        return "refused (cut_binding)" if "cut_binding" in x.get("_text", "") else "refused (" + x.get("_text", "")[:60] + ")"
    have = {t.split("__")[-1] for t in names}
    scopes = [s for t, s in [("cut_get", "read"), ("cut_edit", "edit"), ("export", "render"), ("ai_pass", "ai"), ("see", "read_media")] if t in have]
    summary += f"; another cut: read {said(o)}, edit {said(e)}; scopes {' '.join(scopes)}"
finish(summary)
