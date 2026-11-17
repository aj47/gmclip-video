#!/bin/sh
# A scripted ACP agent for the e2e (decision 0035): no model, no network. It streams a
# reply, calls one gmclip tool and shows a plan; a message with "shell" in it asks
# before running a shell command and waits for the answer. It offers two models.
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":\([0-9]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"sessionCapabilities":{"resume":{}}},"agentInfo":{"name":"fake-acp","version":"0.0.1"}}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"e2e","modes":{"currentModeId":"default","availableModes":[{"id":"default","name":"Ask first"},{"id":"plan","name":"Plan"},{"id":"bypassPermissions","name":"Bypass permissions"}]},"models":{"currentModelId":"fast","availableModels":[{"modelId":"fast","name":"Fast"},{"modelId":"smart","name":"Smart"}]}}}\n' "$id" ;;
    *'"method":"session/resume"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/set_model"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/set_mode"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      say() { printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"e2e","update":%s}}\n' "$1"; }
      say '{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Looking at "}}'
      sleep 0.2
      say '{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"your projects."}}'
      say '{"sessionUpdate":"plan","entries":[{"content":"List the projects","status":"in_progress","priority":"high"},{"content":"Pick the best moment","status":"pending","priority":"medium"}]}'
      say '{"sessionUpdate":"tool_call","toolCallId":"g1","title":"mcp__gmclip__project_list","kind":"other","status":"in_progress","rawInput":{}}'
      sleep 0.2
      say '{"sessionUpdate":"tool_call_update","toolCallId":"g1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"1 project"}}]}'
      # a session on a cut: read it and trim it (the work card shows the cut)
      cut=$(printf '%s' "$line" | sed -n 's/.*session on cut \([0-9A-Z]\{26\}\) of project.*/\1/p')
      if [ -n "$cut" ]; then
        say "{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"c1\",\"title\":\"mcp__gmclip__cut_view\",\"kind\":\"read\",\"status\":\"in_progress\",\"rawInput\":{\"cut\":\"$cut\"}}"
        sleep 0.3
        say '{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed"}'
        say "{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"c2\",\"title\":\"mcp__gmclip__cut_trim\",\"kind\":\"edit\",\"status\":\"in_progress\",\"rawInput\":{\"cut\":\"$cut\"}}"
        sleep 0.6
        say '{"sessionUpdate":"tool_call_update","toolCallId":"c2","status":"completed"}'
      fi
      case "$line" in
        *shell*)
          say '{"sessionUpdate":"tool_call","toolCallId":"x1","title":"`ls ~/Movies`","kind":"execute","status":"pending"}'
          printf '{"jsonrpc":"2.0","id":7001,"method":"session/request_permission","params":{"sessionId":"e2e","toolCall":{"toolCallId":"x1","title":"`ls ~/Movies`","kind":"execute"},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"always","name":"Always allow","kind":"allow_always"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]}}\n'
          IFS= read -r answer
          case "$answer" in
            *'"optionId":"allow"'*|*'"optionId":"always"'*) say '{"sessionUpdate":"tool_call_update","toolCallId":"x1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"stream-14.mp4"}}]}' ;;
            *) say '{"sessionUpdate":"tool_call_update","toolCallId":"x1","status":"failed"}' ;;
          esac ;;
      esac
      say '{"sessionUpdate":"plan","entries":[{"content":"List the projects","status":"completed","priority":"high"},{"content":"Pick the best moment","status":"completed","priority":"medium"}]}'
      say '{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":" Done: the strongest moment is at 12:04."}}'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
