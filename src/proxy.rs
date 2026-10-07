//! The proxy loop: everything the agent says is inspected, and everything the
//! upstream says passes through untouched unless a limit was hit.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::audit::{self, AuditLog};
use crate::config::Config;
use crate::error::{Result, SandmanError};
use crate::policy::{Policy, Verdict};
use crate::protocol::{methods, Message, RpcError, SUPPORTED_PROTOCOL_VERSION};
use crate::upstream::{denied_result, ToolInfo, Upstream};

/// Shared state for one running proxy.
pub struct Proxy {
    config: Config,
    policy: Policy,
    upstream: Arc<Upstream>,
    audit: Option<AuditLog>,
    /// Upstream tools that survived the policy, already renamed for the agent.
    tools: Vec<ToolInfo>,
}

impl Proxy {
    /// Connect to the upstream, apply the policy, and prepare the tool list.
    pub async fn new(config: Config) -> Result<Self> {
        let policy = Policy::compile(&config)?;
        let upstream = Upstream::connect(&config).await?;

        let upstream_tools = upstream.list_tools().await?;
        let tools = apply_tool_policy(&policy, upstream_tools, &config);
        if tools.is_empty() && config.tools.require_non_empty {
            return Err(crate::error::config_err(
                "policy exposed zero tools and `tools.require_non_empty` is set; \
                 check the allow/deny rules against the server's actual tool names",
            ));
        }

        let audit = match &config.audit_log {
            Some(path) => Some(AuditLog::open(path)?),
            None => None,
        };

        tracing::info!(
            server = %config.name,
            isolation = config.isolation.kind(),
            tools_exposed = tools.len(),
            "sandbox ready"
        );

        Ok(Self {
            config,
            policy,
            upstream: Arc::new(upstream),
            audit,
            tools,
        })
    }

    /// Run the stdio proxy until the agent closes stdin or we are killed.
    pub async fn run(&self) -> Result<()> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut stdout = tokio::io::stdout();

        loop {
            let line = tokio::select! {
                result = lines.next_line() => result?,
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("interrupted, shutting down");
                    break;
                }
            };

            let Some(line) = line else { break };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Some(response) = self.handle_line(trimmed).await else {
                continue;
            };

            let mut encoded = serde_json::to_string(&response)?;
            encoded.push('\n');
            stdout.write_all(encoded.as_bytes()).await?;
            stdout.flush().await?;
        }

        self.upstream.shutdown().await;
        Ok(())
    }

    /// Handle one inbound frame.
    ///
    /// Returns `None` for notifications, which by JSON-RPC definition get no
    /// reply — forwarding those and staying silent is the correct behaviour.
    async fn handle_line(&self, line: &str) -> Option<Message> {
        let message: Message = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "unparsable frame from agent");
                return Some(Message::failure(
                    None,
                    RpcError::PARSE_ERROR,
                    format!("could not parse JSON-RPC frame: {e}"),
                ));
            }
        };

        if !message.expects_response() {
            self.forward_notification(&message).await;
            return None;
        }

        let id = message.id.clone().unwrap_or(Value::Null);
        let method = message.method.clone().unwrap_or_default();

        match method.as_str() {
            methods::INITIALIZE => Some(self.handle_initialize(id)),
            methods::TOOLS_LIST => Some(self.handle_tools_list(id)),
            methods::TOOLS_CALL => Some(self.handle_tools_call(id, &method, message.params).await),
            methods::PING => Some(Message::success(Some(id), json!({}))),
            // Resources and prompts pass through untouched: the sandbox's job
            // is to gate tools, not to pretend other capabilities do not exist.
            methods::RESOURCES_LIST | methods::PROMPTS_LIST => self.pass_through(&message).await,
            other => {
                tracing::debug!(method = other, "forwarding unknown method unfiltered");
                self.pass_through(&message).await
            }
        }
    }

    /// Answer `initialize` as the sandbox, not as the upstream.
    ///
    /// We must present our own identity: the agent should know it is talking
    /// through a policy layer, otherwise it cannot reason about what is
    /// filtered or denied.
    fn handle_initialize(&self, id: Value) -> Message {
        Message::success(
            Some(id),
            json!({
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": self.config.name,
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )
    }

    /// Advertise only the tools the policy admits, under their public names.
    fn handle_tools_list(&self, id: Value) -> Message {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.exposed_name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect();
        Message::success(Some(id), json!({ "tools": tools }))
    }

    /// The gate every tool call passes through.
    async fn handle_tools_call(&self, id: Value, _method: &str, params: Option<Value>) -> Message {
        let started = Instant::now();
        let params = params.unwrap_or_else(|| json!({}));
        let exposed = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        if exposed.is_empty() {
            return Message::failure(
                Some(id),
                RpcError::INVALID_PARAMS,
                "tools/call requires a `name` argument",
            );
        }

        let verdict = self.policy.may_call(&exposed);
        if !matches!(verdict, Verdict::Allow) {
            let reason = format!("this sandbox does not expose a tool named `{exposed}`");
            self.record(&exposed, "deny", Some(reason.clone()), &params, started);
            tracing::warn!(tool = %exposed, "denied by tool policy");
            // Denials are reported as a successful JSON-RPC response carrying an
            // error result: the agent should see the refusal as tool output and
            // recover, not treat the whole session as broken.
            return Message::success(Some(id), denied_result(&exposed, &reason));
        }

        if let Some(hit) = self.policy.scan_for_secrets(&arguments) {
            self.record(&exposed, "deny", Some(hit.clone()), &params, started);
            tracing::warn!(tool = %exposed, "denied by secret scanner");
            return Message::success(Some(id), denied_result(&exposed, &hit));
        }

        if let Some(reason) = self.policy.check_resources(&arguments) {
            tracing::warn!(tool = %exposed, %reason, "denied by resource policy");
            self.record(&exposed, "deny", Some(reason.clone()), &params, started);
            return Message::success(Some(id), denied_result(&exposed, &reason));
        }

        let Some(upstream_name) = self.policy.resolve_tool(&exposed) else {
            let reason = format!("`{exposed}` is outside this sandbox's namespace");
            self.record(&exposed, "deny", Some(reason.clone()), &params, started);
            return Message::success(Some(id), denied_result(&exposed, &reason));
        };

        match self
            .upstream
            .call_tool(
                &upstream_name,
                &arguments,
                self.config.limits.call_timeout_ms,
            )
            .await
        {
            Ok(result) => {
                self.record(&exposed, "allow", None, &params, started);
                Message::success(Some(id), truncate_result(&self.config, result))
            }
            Err(e) => {
                tracing::error!(tool = %exposed, error = %e, "upstream call failed");
                self.record(&exposed, "error", Some(e.to_string()), &params, started);
                Message::success(
                    Some(id),
                    RpcError {
                        code: RpcError::INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    }
                    .to_tool_result(),
                )
            }
        }
    }

    /// Forward a method the sandbox does not interpret.
    ///
    /// Resources and prompts are passed through as-is. Refusing them would
    /// break servers that gate work behind them, and the sandbox's job is to
    /// gate *tools*, not to pretend other capabilities do not exist.
    async fn pass_through(&self, message: &Message) -> Option<Message> {
        let id = message.id.clone()?;
        let method = message.method.clone()?;
        let params = message.params.clone().unwrap_or_else(|| json!({}));

        match self
            .upstream
            .forward(&method, &params, self.config.limits.call_timeout_ms)
            .await
        {
            Ok(result) => Some(Message::success(Some(id), result)),
            Err(e) => {
                tracing::warn!(method = %method, error = %e, "pass-through failed");
                Some(Message::success(
                    Some(id),
                    RpcError {
                        code: RpcError::INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    }
                    .to_tool_result(),
                ))
            }
        }
    }

    /// Forward an agent notification to the upstream, ignoring failures.
    ///
    /// Notifications have no reply, so a failure here is logged and dropped.
    async fn forward_notification(&self, message: &Message) {
        let Some(method) = &message.method else {
            return;
        };
        if method == "notifications/cancelled" {
            return;
        }
        tracing::trace!(method, "forwarding notification");
        if let Err(e) = self
            .upstream
            .notify(method, message.params.clone().unwrap_or_else(|| json!({})))
            .await
        {
            tracing::debug!(method, error = %e, "notification was not delivered");
        }
    }

    fn record(
        &self,
        tool: &str,
        decision: &str,
        reason: Option<String>,
        params: &Value,
        started: Instant,
    ) {
        if let Some(log) = &self.audit {
            log.record(&audit::entry(
                tool,
                decision,
                reason,
                params,
                started.elapsed().as_millis() as u64,
            ));
        }
    }
}

/// Cap response size so a runaway upstream cannot exhaust agent context.
///
/// Size is judged on the serialized form, because that is what actually lands
/// in the agent's context window.
fn truncate_result(config: &Config, result: Value) -> Value {
    let encoded_len = serde_json::to_string(&result).map(|s| s.len()).unwrap_or(0);
    let cap = config.limits.max_response_bytes;
    if encoded_len <= cap {
        return result;
    }
    tracing::warn!(
        bytes = encoded_len,
        cap,
        "truncating oversized upstream response"
    );
    json!({
        "content": [{
            "type": "text",
            "text": format!(
                "[sandbox] response truncated at {cap} bytes \
                 (upstream returned {encoded_len}); \
                 narrow the tool call or raise limits.max_response_bytes"
            ),
        }],
        "isError": true,
    })
}

/// Narrow the upstream tool list to what the policy admits, renaming as we go.
fn apply_tool_policy(
    policy: &Policy,
    upstream_tools: Vec<ToolInfo>,
    config: &Config,
) -> Vec<ToolInfo> {
    let mut tools = Vec::new();
    for mut tool in upstream_tools {
        if !policy.tools().admits(&tool.upstream_name) {
            tracing::debug!(tool = %tool.upstream_name, "hidden by policy");
            continue;
        }
        tool.exposed_name = policy.tools().expose_name(&tool.upstream_name);
        tools.push(tool);
    }

    if let Some(ns) = config.tools.namespace.as_deref() {
        if let Some(clash) = find_duplicate(&tools) {
            tracing::error!(
                tool = %clash,
                namespace = ns,
                "renaming produced a duplicate tool name; upstream tools collide"
            );
        }
    }
    tools
}

fn find_duplicate(tools: &[ToolInfo]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    for tool in tools {
        if !seen.insert(tool.exposed_name.clone()) {
            return Some(tool.exposed_name.clone());
        }
    }
    None
}

/// Build a proxy just far enough to learn what it would expose, without
/// serving anything. Backs `sandman doctor`.
pub async fn probe(config: &Config) -> Result<Vec<String>> {
    let proxy = Proxy::new(config.clone()).await?;
    Ok(proxy.tools.iter().map(|t| t.exposed_name.clone()).collect())
}

impl From<SandmanError> for std::io::Error {
    fn from(e: SandmanError) -> Self {
        std::io::Error::other(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ToolPolicy;

    fn test_config() -> Config {
        Config {
            name: "test".into(),
            upstream: crate::config::UpstreamSpec::Stdio {
                command: "cat".into(),
                args: vec![],
                env: Default::default(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn tool_policy_narrows_and_renames() {
        let cfg = Config {
            tools: ToolPolicy {
                allow: vec!["read_*".into()],
                deny: vec!["read_secret".into()],
                namespace: Some("fs__".into()),
                ..Default::default()
            },
            ..test_config()
        };
        let policy = Policy::compile(&cfg).unwrap();
        let upstream = vec![
            ToolInfo {
                upstream_name: "read_file".into(),
                exposed_name: "read_file".into(),
                description: String::new(),
                input_schema: json!({}),
            },
            ToolInfo {
                upstream_name: "read_secret".into(),
                exposed_name: "read_secret".into(),
                description: String::new(),
                input_schema: json!({}),
            },
            ToolInfo {
                upstream_name: "write_file".into(),
                exposed_name: "write_file".into(),
                description: String::new(),
                input_schema: json!({}),
            },
        ];

        let tools = apply_tool_policy(&policy, upstream, &cfg);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].upstream_name, "read_file");
        assert_eq!(tools[0].exposed_name, "fs__read_file");
    }

    #[test]
    fn truncation_replaces_oversized_responses() {
        let cfg = Config {
            limits: crate::config::Limits {
                max_response_bytes: 16,
                ..Default::default()
            },
            ..test_config()
        };
        let out = truncate_result(
            &cfg,
            json!({
                "content": [{"type": "text", "text": "x".repeat(1000)}]
            }),
        );
        assert_eq!(out["isError"], json!(true));
        assert!(out["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("truncated"));
    }

    #[test]
    fn small_responses_pass_through_untouched() {
        let small = json!({"content": "hi"});
        assert_eq!(truncate_result(&test_config(), small.clone()), small);
    }
}
