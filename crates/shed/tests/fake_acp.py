#!/usr/bin/env python3
"""A fake ACP v1 agent for shed's tests: a scripted JSON-RPC server on stdio.

It checks every client message against the fields the ACP schema requires and exits 3 (with the
reason on stderr) on the first that does not fit. Prompts:

- "tool": a plan, a mode change, a tool call and a permission request; then the tool completes and the
  reply is "wrote notes.txt" (allowed), or it fails and the reply is "denied" (rejected).
- "slow": one chunk ("thinking"), then waits for session/cancel and stops with `cancelled`.
- anything else: the reply "you said: <prompt>".
"""
import json
import sys

SESSION = "sess-1"


def fail(why):
    sys.stderr.write(f"fake_acp: {why}\n")
    sys.exit(3)


def check(cond, why):
    if not cond:
        fail(why)


def send(msg):
    msg["jsonrpc"] = "2.0"
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def update(u):
    send({"method": "session/update", "params": {"sessionId": SESSION, "update": u}})


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    msg = json.loads(line)
    check(msg.get("jsonrpc") == "2.0", f"not JSON-RPC 2.0: {line}")
    if msg.get("method") == "session/cancel":
        check(msg.get("id") is None, "session/cancel must be a notification")
        check(msg["params"].get("sessionId") == SESSION, "session/cancel: sessionId")
    return msg


def wait_response(rid):
    """Reads until the response to our request `rid`; True if a cancel came first."""
    cancelled = False
    while True:
        msg = read()
        if msg.get("method") == "session/cancel":
            cancelled = True
            continue
        if msg.get("id") == rid and "method" not in msg:
            outcome = msg["result"]["outcome"]
            check(outcome["outcome"] in ("selected", "cancelled"), f"outcome: {outcome}")
            if outcome["outcome"] == "selected":
                check(isinstance(outcome.get("optionId"), str), "selected needs optionId")
            return outcome, cancelled


def tool_turn():
    update({"sessionUpdate": "plan", "entries": [
        {"content": "Write notes", "priority": "high", "status": "in_progress"}]})
    update({"sessionUpdate": "current_mode_update", "currentModeId": "code"})
    update({"sessionUpdate": "tool_call", "toolCallId": "call-1", "title": "Write notes.txt",
            "kind": "edit", "status": "pending"})
    send({"id": 100, "method": "session/request_permission", "params": {
        "sessionId": SESSION,
        "toolCall": {"toolCallId": "call-1"},
        "options": [
            {"optionId": "allow-once", "name": "Allow", "kind": "allow_once"},
            {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
        ]}})
    outcome, _ = wait_response(100)
    if outcome["outcome"] == "cancelled":
        return "cancelled"
    if outcome["optionId"] == "allow-once":
        update({"sessionUpdate": "tool_call_update", "toolCallId": "call-1", "status": "completed"})
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "wrote "}})
        update({"sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "notes.txt"}})
    else:
        update({"sessionUpdate": "tool_call_update", "toolCallId": "call-1", "status": "failed"})
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "denied"}})
    return "end_turn"


def main():
    while True:
        msg = read()
        method, params, rid = msg.get("method"), msg.get("params", {}), msg.get("id")
        if method == "initialize":
            check(isinstance(rid, int), "initialize: id")
            check(params.get("protocolVersion") == 1, "initialize: protocolVersion 1")
            check(isinstance(params.get("clientCapabilities"), dict), "initialize: clientCapabilities")
            send({"id": rid, "result": {"protocolVersion": 1, "agentCapabilities": {},
                                        "authMethods": []}})
        elif method == "session/new":
            check(str(params.get("cwd", "")).startswith("/"), "session/new: absolute cwd")
            check(params.get("mcpServers") == [], "session/new: mcpServers")
            send({"id": rid, "result": {"sessionId": SESSION}})
        elif method == "session/prompt":
            check(params.get("sessionId") == SESSION, "session/prompt: sessionId")
            blocks = params.get("prompt")
            check(isinstance(blocks, list) and blocks, "session/prompt: prompt blocks")
            check(all(b.get("type") == "text" and isinstance(b.get("text"), str) for b in blocks),
                  "session/prompt: text blocks")
            text = blocks[0]["text"]
            if text == "tool":
                stop = tool_turn()
            elif text == "slow":
                update({"sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "thinking"}})
                while read().get("method") != "session/cancel":
                    pass
                stop = "cancelled"
            else:
                update({"sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": f"you said: {text}"}})
                stop = "end_turn"
            send({"id": rid, "result": {"stopReason": stop}})
        elif method == "session/cancel":
            pass
        else:
            fail(f"unexpected message: {msg}")


main()
