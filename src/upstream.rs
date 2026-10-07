//! Connecting to the upstream MCP server.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::config::{Config, Isolation, UpstreamSpec};
use crate::error::{config_err, Result, SandmanError};
use crate::protocol::{initialize_params, methods, Message};

/// What the sandbox knows about one upstream tool.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub upstream_name: String,
    pub exposed_name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A live connection to the upstream server.
pub enum Upstream {
    Stdio(StdioConnection),
    #[cfg(feature = "http")]
    Http(HttpConnection),
}

impl Upstream {
    /// Open a connection using the strategy described in `config`.
    pub async fn connect(config: &Config) -> Result<Self> {
        match &config.upstream {
            UpstreamSpec::Stdio { command, args, env } => Ok(Self::Stdio(
                StdioConnection::spawn(config, command, args, env).await?,
            )),
            #[cfg(feature = "http")]
            UpstreamSpec::Http { url, headers } => Ok(Self::Http(HttpConnection::new(
                url.clone(),
                headers.clone(),
            ))),
            #[cfg(not(feature = "http"))]
            UpstreamSpec::Http { .. } => Err(SandmanError::HttpDisabled),
        }
    }

    /// Fetch the upstream tool list.
    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        match self {
            Self::Stdio(c) => c.list_tools().await,
            #[cfg(feature = "http")]
            Self::Http(c) => c.list_tools().await,
        }
    }

    /// Invoke a tool by its *upstream* name.
    pub async fn call_tool(&self, name: &str, args: &Value, timeout_ms: u64) -> Result<Value> {
        match self {
            Self::Stdio(c) => c.call_tool(name, args, timeout_ms).await,
            #[cfg(feature = "http")]
            Self::Http(c) => c.call_tool(name, args, timeout_ms).await,
        }
    }

    /// Forward a method the sandbox does not interpret (resources, prompts).
    pub async fn forward(&self, method: &str, params: &Value, timeout_ms: u64) -> Result<Value> {
        match self {
            Self::Stdio(c) => c.request(method, params.clone(), timeout_ms).await,
            #[cfg(feature = "http")]
            Self::Http(c) => c.forward(method, params, timeout_ms).await,
        }
    }

    /// Send a notification upstream. No response is expected or awaited.
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        match self {
            Self::Stdio(c) => {
                c.send(&Message::notification(method, params)).await?;
                Ok(())
            }
            #[cfg(feature = "http")]
            Self::Http(_) => Ok(()),
        }
    }

    /// Best-effort shutdown.
    pub async fn shutdown(&self) {
        match self {
            Self::Stdio(c) => c.terminate().await,
            #[cfg(feature = "http")]
            Self::Http(c) => c.shutdown().await,
        }
    }
}

/// Build the base [`Command`] with Windows's `cmd` shim disabled.
///
/// Going through `cmd /c` to spawn a child lets an argument like `& calc`
/// escape into a second command. Turning that off keeps the upstream argv
/// literal.
fn base_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    // Suppress the console window a child would otherwise pop up, which on a
    // headless agent host is both noise and a visible side effect.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.kill_on_drop(true);
    cmd
}

/// A child process speaking newline-delimited JSON-RPC on stdio.
pub struct StdioConnection {
    stdin: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
    child: Arc<Mutex<Option<Child>>>,
    next_id: std::sync::atomic::AtomicU64,
    pending: std::sync::atomic::AtomicU64,
}

impl StdioConnection {
    async fn spawn(
        config: &Config,
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut cmd = match &config.isolation {
            Isolation::None => base_command(command),
            Isolation::Container { image, args: extra } => {
                let mut c = base_command("docker");
                c.arg("run").arg("--rm").arg("-i");
                // The sandbox proxies stdio, so the container needs no TTY and
                // must not publish ports unless the policy explicitly wants it.
                for arg in extra {
                    c.arg(arg);
                }
                c.arg(image).arg(command);
                c
            }
        };
        cmd.args(args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // A sandboxed server must not inherit the operator's full environment:
        // that is exactly where the secrets it might exfiltrate live. Pass
        // through PATH/HOME plus whatever the policy names.
        cmd.env_clear();
        for key in [
            "PATH",
            "HOME",
            "LANG",
            "TMPDIR",
            "SystemRoot",
            "TEMP",
            "TMP",
        ] {
            let value = match key {
                "HOME" => std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
                "SystemRoot" => std::env::var_os("SystemRoot"),
                "TEMP" => std::env::var_os("TEMP").or_else(|| std::env::var_os("TMPDIR")),
                "PATH" => std::env::var_os("PATH"),
                other => std::env::var_os(other),
            };
            if let Some(value) = value {
                cmd.env(key, value);
            }
        }
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            config_err(format!(
                "cannot start upstream `{command}`: {e}. Is it on PATH?"
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| config_err("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| config_err("no stdout"))?;

        // Drain stderr on a background task so a chatty server cannot deadlock
        // on a full pipe buffer, and so its warnings reach our logs.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let lines = BufReader::new(stderr).lines();
                tokio::pin!(lines);
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "upstream::stderr", "{line}");
                }
            });
        }

        let conn = Self {
            stdin: Mutex::new(stdin),
            reader: Mutex::new(BufReader::new(stdout)),
            child: Arc::new(Mutex::new(Some(child))),
            next_id: std::sync::atomic::AtomicU64::new(1),
            pending: std::sync::atomic::AtomicU64::new(0),
        };

        conn.handshake(config.limits.init_timeout_ms).await?;
        Ok(conn)
    }

    /// Perform the MCP initialize handshake.
    async fn handshake(&self, timeout_ms: u64) -> Result<()> {
        let id = self.take_id();
        let params = initialize_params("mcp-sandman");
        self.send(&Message::request(json!(id), methods::INITIALIZE, params))
            .await?;
        let response = self.recv_for(id, timeout_ms).await?;

        if let Some(err) = response.error {
            return Err(SandmanError::Config(format!(
                "upstream rejected initialize: {}",
                err.message
            )));
        }

        // Tell the server the handshake is complete. Per MCP this is a
        // notification and gets no reply.
        self.send(&Message::notification(methods::INITIALIZED, json!({})))
            .await?;
        Ok(())
    }

    /// Send one framed message to the upstream.
    pub(crate) async fn send(&self, msg: &Message) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Read frames until the response matching `id` arrives.
    ///
    /// Server-initiated requests and unrelated notifications are skipped:
    /// they have ids we did not issue, so they are not our answer.
    async fn recv_for(&self, id: u64, timeout_ms: u64) -> Result<Message> {
        let read = async {
            let mut reader = self.reader.lock().await;
            loop {
                let mut line = String::new();
                let n = reader.read_line(&mut line).await?;
                if n == 0 {
                    return Err(SandmanError::UpstreamExited);
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Message>(trimmed) {
                    Ok(msg) => {
                        if msg.id.as_ref().and_then(Value::as_u64) == Some(id) {
                            return Ok(msg);
                        }
                        tracing::trace!(method = ?msg.method, "skipping unrelated upstream frame");
                    }
                    Err(e) => tracing::warn!(error = %e, "discarding unparsable upstream frame"),
                }
            }
        };

        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), read).await {
            Ok(result) => result,
            Err(_) => Err(SandmanError::UpstreamTimeout(timeout_ms)),
        }
    }

    fn take_id(&self) -> u64 {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Issue a request and wait for the matching response.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value> {
        let id = self.take_id();
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = async {
            self.send(&Message::request(json!(id), method, params))
                .await?;
            let response = self.recv_for(id, timeout_ms).await?;
            match response.error {
                Some(err) => Err(SandmanError::PolicyViolation(err.message)),
                None => Ok(response.result.unwrap_or(Value::Null)),
            }
        }
        .await;
        self.pending
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        result
    }

    async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let result = self.request(methods::TOOLS_LIST, json!({}), 30_000).await?;
        Ok(parse_tool_list(&result))
    }

    async fn call_tool(&self, name: &str, args: &Value, timeout_ms: u64) -> Result<Value> {
        self.request(
            methods::TOOLS_CALL,
            json!({ "name": name, "arguments": args }),
            timeout_ms,
        )
        .await
    }

    /// Ask the child to exit, then kill it if it refuses.
    pub async fn terminate(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await;
        }
    }
}

/// Normalize an upstream `tools/list` result into [`ToolInfo`].
///
/// Servers disagree on casing and shape, so we accept both `inputSchema` and
/// `input_schema` and treat a missing schema as "any object".
pub fn parse_tool_list(result: &Value) -> Vec<ToolInfo> {
    let tools = match result.get("tools").and_then(Value::as_array) {
        Some(tools) => tools,
        None => return Vec::new(),
    };

    tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            Some(ToolInfo {
                upstream_name: name.to_string(),
                exposed_name: name.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: tool
                    .get("inputSchema")
                    .or_else(|| tool.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
        })
        .collect()
}

/// Build the error result returned to the agent when a call is refused.
///
/// `reason` must say *why* the call was refused. Telling an agent that a tool
/// "is not available" when the tool exists but its arguments broke the policy
/// sends it looking for a different tool instead of fixing its call.
pub fn denied_result(tool: &str, reason: &str) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": format!(
                "sandbox refused `{tool}`: {reason}. \
                 Adjust the call, or ask the operator to widen the policy in sandman.toml."
            ),
        }],
        "isError": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_camel_and_snake_schema() {
        let result = json!({
            "tools": [
                {"name": "a", "description": "d", "inputSchema": {"type": "object"}},
                {"name": "b", "input_schema": {"type": "object", "required": ["x"]}},
            ]
        });
        let tools = parse_tool_list(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].input_schema["type"], "object");
        assert_eq!(tools[1].input_schema["required"][0], "x");
    }

    #[test]
    fn tolerates_missing_description_and_schema() {
        let result = json!({"tools": [{"name": "solo"}]});
        let tools = parse_tool_list(&result);
        assert_eq!(tools[0].description, "");
        assert_eq!(tools[0].input_schema["type"], "object");
    }

    #[test]
    fn skips_malformed_tool_entries() {
        let result = json!({"tools": [{"description": "no name"}, {"name": "ok"}]});
        let tools = parse_tool_list(&result);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].upstream_name, "ok");
    }

    #[test]
    fn empty_result_yields_no_tools() {
        assert!(parse_tool_list(&json!({})).is_empty());
    }
}
