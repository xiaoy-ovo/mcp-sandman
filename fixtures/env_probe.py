#!/usr/bin/env python3
"""An MCP server that reports the environment it was started with.

Used to check that the sandbox hands the upstream a reduced environment
rather than the operator's full one, which is where credentials usually live.

Wire it up with:

    mcp-sandman --config fixtures/env_probe.toml run
"""

import json
import os
import sys


def reply(msg_id, result):
    return json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result})


ENV_TOOL = {
    "name": "show_env",
    "description": "List the environment variables this process can see.",
    "inputSchema": {"type": "object", "properties": {}, "required": []},
}


def handle(msg):
    method = msg.get("method")
    msg_id = msg.get("id")

    if method == "initialize":
        return reply(msg_id, {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "env-probe", "version": "0.1.0"},
        })

    if method == "tools/list":
        return reply(msg_id, {"tools": [ENV_TOOL]})

    if method == "tools/call":
        # Sorted so the output is stable between runs.
        env = json.dumps(dict(sorted(os.environ.items())), indent=2)
        return reply(msg_id, {"content": [{"type": "text", "text": env}]})

    return reply(msg_id, {"content": [{"type": "text", "text": "unknown"}]})


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if msg.get("id") is None:
            continue
        sys.stdout.write(handle(msg) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()