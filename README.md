# mcp-sandman

**A policy-enforcing sandbox proxy for Model Context Protocol servers.**

Point your agent at `mcp-sandman` instead of at the MCP server directly. Every
tool list, every tool call, every path and hostname passes through a policy you
write, and anything the policy does not allow is refused before it reaches the
server.

```
agent ──stdio──▶ mcp-sandman ──stdio──▶ your MCP server
                   │
                   ├── which tools exist
                   ├── which paths may be read or written
                   ├── which hosts may be reached
                   └── an audit line for every decision
```

## Why

You installed a third-party MCP server so your agent could read a database. It
can also `read_file`, because nothing stopped it. The server is running with
your credentials, in your shell, next to your SSH keys.

Most MCP tooling assumes the server is trustworthy and the agent is not. That
assumption breaks down exactly when you install something you have not audited.

mcp-sandman inverts it: the server is treated as hostile, and the policy — not
the server — decides what happens.

## Install

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/xiaoy-ovo/mcp-sandman/main/install.sh | sh

# or from source
cargo install --path .
```

One static binary, no runtime dependencies. Windows: `cargo build --release`
and take `target/release/mcp-sandman.exe`.

## Use

Generate a starter policy:

```bash
mcp-sandman init "npx -y @some/package" > sandman.toml
```

Check what survives the policy:

```bash
$ mcp-sandman --config sandman.toml doctor
2 tool(s) exposed:
  read_file
  fetch_url
```

Wire it into your agent's MCP config:

```json
{
  "mcpServers": {
    "db": {
      "command": "mcp-sandman",
      "args": ["--config", "/path/to/sandman.toml"]
    }
  }
}
```

From here the agent sees only the two tools above, cannot write files, and can
only reach the hosts you listed.

## Policy

```toml
name = "db-sandbox"

# Top-level keys must come before any table.
audit_log = "./audit.log"

[upstream]
transport = "stdio"
command = "npx"
args = ["-y", "@acme/db-mcp"]

[tools]
allow = ["query_*", "describe_*"]   # empty = everything the server exposes
deny  = ["drop_*", "*_admin"]
require_non_empty = true            # refuse to start if the policy hides all tools

[filesystem]
read  = ["**"]                      # relative to the working directory
write = []                          # read-only by default

[network]
allow_hosts = ["*.internal.corp"]
allow_ports = [443]

[limits]
call_timeout_ms = 30000
max_response_bytes = 8388608

# Refuse calls whose arguments look like credentials.
secret_patterns = ['sk-[A-Za-z0-9]{20,}']
```

`mcp-sandman check --config sandman.toml` validates it without connecting.
`mcp-sandman doctor` connects, lists what survives, and is the fastest way to
find a typo in a tool name.

### Two rules worth knowing

**Absolute paths are refused unless the policy names absolute paths.** A policy
of `read = ["**"]` matches relative paths only. This is deliberate: `**` in a
glob engine matches across `/`, so without this rule `read = ["**"]` would
silently permit `/etc/shadow`. Write `["/**"]` when you really do mean
everything.

**The audit log records argument names, never values.** An audit trail that
stores the arguments it recorded is itself a place secrets leak to.

## Commands

| Command | What it does |
|---|---|
| `mcp-sandman` (or `run`) | Serve on stdio. The default. |
| `mcp-sandman check` | Validate the policy. Connects to nothing. |
| `mcp-sandman doctor` | Connect and list the tools that survive. |
| `mcp-sandman init <cmd>` | Print a starter policy for a command. |

Logs go to **stderr**, always. stdout carries the JSON-RPC stream.

## What this is not

mcp-sandman inspects the *arguments* an agent sends. A server that builds a
path at runtime, or reads a file the agent never named, is not stopped by
argument inspection. For an untrusted server, pair this with the container
isolation mode:

```toml
[isolation]
isolation = "container"
image = "node:22-slim"
args = ["--network=none", "--read-only"]
```

That is the layer that holds regardless of what the server does. mcp-sandman is
the layer that is easy to adopt, because it needs no container runtime.

## Safety notes

- The upstream process starts with a **cleared environment**. It gets `PATH`,
  `HOME` and locale, plus whatever `[upstream.env]` names — not the full
  environment of whatever launched the agent.
- Denials are returned as tool *results* with `isError: true`, not as protocol
  errors. The agent can read what was refused and adjust; the session survives.
- Denial messages say **why**. "Path X is outside this policy" sends the agent
  looking for a different path; "tool not available" sends it looking for a
  different tool.

## Development

```bash
cargo test              # 43 tests
cargo build --release
python fixtures/insecure_server.py    # a deliberately unsafe MCP server
mcp-sandman --config fixtures/insecure.toml doctor
```

`fixtures/insecure_server.py` exposes `read_file`, `write_file`, `fetch_url` and
`delete_everything` with no checks of its own. It exists so the tests and the
README describe a real, reproducible result rather than a hoped-for one.

## License

MIT

## Credits

The design draws on prior work in this space:
[pro-vi/mcp-filter](https://github.com/pro-vi/mcp-filter) for the proxy-as-
middleware shape and config-driven tool rules,
[Automata-Labs/code-sandbox-mcp](https://github.com/Automata-Labs-team/code-sandbox-mcp)
for container lifecycle handling, and the
[Model Context Protocol](https://modelcontextprotocol.io) specification for the
protocol itself. All code here is original.