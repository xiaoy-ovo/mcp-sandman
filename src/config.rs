//! Declarative policy: what a sandboxed MCP server is allowed to touch.
//!
//! Everything the sandbox enforces is described here. The config is layered:
//! defaults < file < environment < CLI flags, so a checked-in `sandman.toml`
//! can be tightened per-developer without editing the server itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{config_err, Result};

/// Filesystem policy for the sandboxed process.
///
/// `read` defaults to `["**"]` and `write` defaults to `[]`: a sandbox that can
/// be read but never modified is the safe starting point, so we open read-wide
/// and write-narrow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FsPolicy {
    /// Glob patterns the sandbox may read. Empty means "read nothing".
    pub read: Vec<String>,
    /// Glob patterns the sandbox may write. Empty means "write nothing".
    pub write: Vec<String>,
    /// When false, a denied path is an error rather than an empty result.
    pub deny_by_default: bool,
}

impl Default for FsPolicy {
    fn default() -> Self {
        Self {
            read: vec!["**".to_string()],
            write: Vec::new(),
            deny_by_default: true,
        }
    }
}

/// Network policy for the sandboxed process.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetPolicy {
    /// Hostname patterns the sandbox may reach. Empty means "no network".
    pub allow_hosts: Vec<String>,
    /// Explicitly blocked hosts, applied after `allow_hosts`.
    pub deny_hosts: Vec<String>,
    /// Ports the sandbox may connect to. Empty means "any port on an allowed host".
    pub allow_ports: Vec<u16>,
    /// Drop all outbound DNS resolution.
    pub block_dns: bool,
}

impl NetPolicy {
    /// Whether any outbound connection is possible at all.
    pub fn is_offline(&self) -> bool {
        self.allow_hosts.is_empty() && self.allow_ports.is_empty()
    }
}

/// How the sandboxed server is launched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "isolation", rename_all = "lowercase")]
pub enum Isolation {
    /// Run the command directly, applying only in-process policy checks.
    /// Fast and dependency-free, but a hostile server can ignore them.
    #[default]
    None,
    /// Run inside a container image, applying real kernel-level isolation.
    Container {
        image: String,
        /// Extra `docker run` arguments, e.g. `["--network=none"]`.
        #[serde(default)]
        args: Vec<String>,
    },
}

impl Isolation {
    /// Human-readable label for logs and `sandman doctor` output.
    pub fn kind(&self) -> &'static str {
        match self {
            Isolation::None => "none",
            Isolation::Container { .. } => "container",
        }
    }
}

/// Per-tool gating. Every tool the upstream advertises must survive this.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolPolicy {
    /// Tool names or regex patterns that are exposed to the agent.
    pub allow: Vec<String>,
    /// Tools that are removed even if they match `allow`.
    pub deny: Vec<String>,
    /// Prefix prepended to every exposed tool name, e.g. `"github__"`.
    /// Lets several sandboxes coexist without the agent confusing their tools.
    pub namespace: Option<String>,
    /// Refuse to start when a policy would expose zero tools.
    pub require_non_empty: bool,
}

/// Timeouts, applied at every layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Milliseconds to wait for the upstream `initialize` handshake.
    pub init_timeout_ms: u64,
    /// Milliseconds to wait for any single tool call.
    pub call_timeout_ms: u64,
    /// Maximum response body the sandbox will buffer, in bytes.
    pub max_response_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            init_timeout_ms: 30_000,
            call_timeout_ms: 120_000,
            max_response_bytes: 8 * 1024 * 1024,
        }
    }
}

/// The full policy for one sandboxed server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Name surfaced to the agent as the MCP server name.
    pub name: String,
    /// How to reach the upstream server.
    pub upstream: UpstreamSpec,
    /// Tool-level gating.
    pub tools: ToolPolicy,
    /// Filesystem limits.
    pub filesystem: FsPolicy,
    /// Network limits.
    pub network: NetPolicy,
    /// Process isolation strategy.
    pub isolation: Isolation,
    /// Timeouts and size caps.
    pub limits: Limits,
    /// `tracing` filter directive, e.g. `info,mcp_sandman=debug`.
    pub log_level: String,
    /// Write an audit record for every tool call to this path.
    pub audit_log: Option<PathBuf>,
    /// Refuse to forward requests whose argument names match these patterns.
    /// Guards against an agent being talked into exfiltrating a secret.
    pub secret_patterns: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "mcp-sandman".to_string(),
            upstream: UpstreamSpec::default(),
            tools: ToolPolicy::default(),
            filesystem: FsPolicy::default(),
            network: NetPolicy::default(),
            isolation: Isolation::default(),
            limits: Limits::default(),
            log_level: "info".to_string(),
            audit_log: None,
            secret_patterns: Vec::new(),
        }
    }
}

impl Config {
    /// Read and validate a config file, layering it on top of the defaults.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| config_err(format!("cannot read {}: {e}", path.display())))?;
        let cfg: Config =
            toml::from_str(&text).map_err(|e| config_err(format!("{}: {e}", path.display())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject configs that are internally inconsistent or obviously unsafe.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(config_err("`name` must not be empty"));
        }

        match &self.upstream {
            UpstreamSpec::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(config_err(
                    "`upstream.command` must not be empty when transport is stdio",
                ));
            }
            UpstreamSpec::Http { url, .. } if url.trim().is_empty() => {
                return Err(config_err(
                    "`upstream.url` must not be empty when transport is http",
                ));
            }
            _ => {}
        }

        if let Some(ns) = &self.tools.namespace {
            if !ns.ends_with('_') {
                tracing::warn!(
                    namespace = ns,
                    "namespace does not end in an underscore; exposed tool names may \
                     collide with each other"
                );
            }
        }

        Ok(())
    }

    /// Compile the tool allow/deny rules into matchers.
    pub fn tool_rules(&self) -> Result<ToolRules> {
        ToolRules::compile(&self.tools)
    }

    /// Compile the secret-scanning patterns.
    pub fn secret_rules(&self) -> Result<Vec<Regex>> {
        self.secret_patterns
            .iter()
            .map(|p| {
                Regex::new(p)
                    .map_err(|e| config_err(format!("invalid secret_patterns entry `{p}`: {e}")))
            })
            .collect()
    }
}

/// How the sandbox reaches the upstream MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum UpstreamSpec {
    /// Spawn the server as a child process and speak JSON-RPC over stdio.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Environment variables passed to the child. By default the child
        /// inherits a *reduced* environment, not the parent's.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// Connect to a remote server over Streamable HTTP.
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl Default for UpstreamSpec {
    fn default() -> Self {
        UpstreamSpec::Stdio {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }
}

/// Compiled form of [`ToolPolicy`].
pub struct ToolRules {
    allow_exact: Vec<String>,
    allow_globs: GlobSet,
    deny_exact: Vec<String>,
    deny_globs: GlobSet,
    namespace: Option<String>,
}

impl ToolRules {
    fn compile(policy: &ToolPolicy) -> Result<Self> {
        let mut allow = GlobSetBuilder::new();
        let mut deny = GlobSetBuilder::new();
        let mut allow_exact = Vec::new();
        let mut deny_exact = Vec::new();

        for rule in &policy.allow {
            if rule.contains(['*', '?', '[']) {
                allow.add(compile_glob(rule)?);
            } else {
                allow_exact.push(rule.clone());
            }
        }
        for rule in &policy.deny {
            if rule.contains(['*', '?', '[']) {
                deny.add(compile_glob(rule)?);
            } else {
                deny_exact.push(rule.clone());
            }
        }

        Ok(Self {
            allow_exact,
            allow_globs: allow.build().map_err(|e| config_err(e.to_string()))?,
            deny_exact,
            deny_globs: deny.build().map_err(|e| config_err(e.to_string()))?,
            namespace: policy.namespace.clone(),
        })
    }

    /// Decide whether a tool survives the policy.
    ///
    /// `allow` empty means "allow everything not denied", which matches how
    /// most people read an empty allowlist: no explicit restriction.
    pub fn admits(&self, name: &str) -> bool {
        if self.deny_exact.iter().any(|d| d == name) || self.deny_globs.is_match(name) {
            return false;
        }
        if self.allow_exact.is_empty() && self.allow_globs.is_empty() {
            return true;
        }
        self.allow_exact.iter().any(|a| a == name) || self.allow_globs.is_match(name)
    }

    /// The name the agent sees for a given upstream tool.
    pub fn expose_name(&self, upstream_name: &str) -> String {
        match &self.namespace {
            Some(ns) => format!("{ns}{upstream_name}"),
            None => upstream_name.to_string(),
        }
    }

    /// Recover the upstream name from a name the agent used, if namespaced.
    pub fn upstream_name<'a>(&self, exposed: &'a str) -> Option<&'a str> {
        match &self.namespace {
            None => Some(exposed),
            Some(ns) => exposed.strip_prefix(ns.as_str()),
        }
    }

    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
}

fn compile_glob(pattern: &str) -> Result<Glob> {
    Glob::new(pattern).map_err(|e| config_err(format!("invalid glob `{pattern}`: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(allow: &[&str], deny: &[&str]) -> ToolRules {
        let policy = ToolPolicy {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            namespace: None,
            require_non_empty: false,
        };
        ToolRules::compile(&policy).unwrap()
    }

    #[test]
    fn empty_allow_permits_everything_unless_denied() {
        let r = rules(&[], &["rm_rf"]);
        assert!(r.admits("read_file"));
        assert!(!r.admits("rm_rf"));
    }

    #[test]
    fn deny_beats_allow() {
        let r = rules(&["read_*"], &["read_secret"]);
        assert!(r.admits("read_file"));
        assert!(!r.admits("read_secret"));
    }

    #[test]
    fn namespace_roundtrips() {
        let policy = ToolPolicy {
            namespace: Some("gh__".into()),
            ..Default::default()
        };
        let r = ToolRules::compile(&policy).unwrap();
        let exposed = r.expose_name("create_issue");
        assert_eq!(exposed, "gh__create_issue");
        assert_eq!(r.upstream_name(&exposed), Some("create_issue"));
    }

    #[test]
    fn upstream_name_rejects_foreign_tool() {
        let policy = ToolPolicy {
            namespace: Some("gh__".into()),
            ..Default::default()
        };
        let r = ToolRules::compile(&policy).unwrap();
        assert_eq!(r.upstream_name("create_issue"), None);
    }

    #[test]
    fn invalid_glob_is_reported() {
        let policy = ToolPolicy {
            allow: vec!["read_[".into()],
            ..Default::default()
        };
        assert!(ToolRules::compile(&policy).is_err());
    }

    #[test]
    fn stdio_upstream_needs_a_command() {
        let cfg = Config::default();
        assert!(cfg.validate().is_err());
    }
}
