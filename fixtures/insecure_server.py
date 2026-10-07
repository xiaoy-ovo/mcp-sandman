#!/usr/bin/env python3
"""A deliberately insecure MCP server, used to prove mcp-sandman gates it.

Speaks newline-delimited JSON-RPC on stdio, like a real MCP server, and
offers tools that touch the filesystem and the network without asking.

Run it under the sandbox to see what gets through:

    mcp-sandman --config fixtures/insecure.toml
"""

import json
import sys
import urllib.request

TOOLS = [
    {
        "name": "read_file",
        "description": "Read any file on this host.",
        "inputSchema": {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        },
    },
    {
        "name": "write_file",
        "description": "Write to any file on this host.",
        "inputSchema": {
            "type": "object",
            "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
            "required": ["path", "content"],
        },
    },
    {
        "name": "fetch_url",
        "description": "Fetch any URL and return the body.",
        "inputSchema": {
            "type": "object",
            "properties": {"url": {"type": "string"}},
            "required": ["url"],
        },
    },
    {
        "name": "delete_everything",
        "description": "Dangerous: removes a path.",
        "inputSchema": {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        },
    },
]


def reply(msg_id, result):
    return json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result})


def handle(msg):
    method = msg.get("method")
    msg_id = msg.get("id")

    if method == "initialize":
        return reply(msg_id, {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "insecure-fs", "version": "0.1.0"},
        })

    if method == "tools/list":
        return reply(msg_id, {"tools": TOOLS})

    if method == "tools/call":
        params = msg.get("params", {})
        name = params.get("name")
        args = params.get("arguments", {})

        # No checks here on purpose: the whole point is that the sandbox is
        # what stands between the agent and these operations.
        if name == "read_file":
            with open(args["path"], "r", encoding="utf-8", errors="replace") as fh:
                return reply(msg_id, {
                    "content": [{"type": "text", "text": fh.read()[:4000]}]
                })

        if name == "write_file":
            with open(args["path"], "w", encoding="utf-8") as fh:
                fh.write(args.get("content", ""))
            return reply(msg_id, {"content": [{"type": "text", "text": "written"}]})

        if name == "fetch_url":
            with urllib.request.urlopen(args["url"], timeout=5) as resp:
                body = resp.read(2000).decode("utf-8", errors="replace")
            return reply(msg_id, {"content": [{"type": "text", "text": body}]})

        if name == "delete_everything":
            import os
            os.remove(args["path"])
            return reply(msg_id, {"content": [{"type": "text", "text": "deleted"}]})

    return reply(msg_id, {"content": [{"type": "text", "text": f"unknown tool {name}"}]})


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        # Notifications carry no id and get no reply.
        if msg.get("id") is None:
            continue
        sys.stdout.write(handle(msg) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()