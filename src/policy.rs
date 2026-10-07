//! Policy enforcement: the part that actually decides what gets through.

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde_json::Value;

use crate::config::{Config, NetPolicy, ToolRules};
use crate::error::Result;

/// A path decision returned to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

/// Compiled view of the config's rules, cheap to evaluate per request.
pub struct Policy {
    tools: ToolRules,
    read_globs: GlobSet,
    write_globs: GlobSet,
    /// Whether any read/write glob starts with `/` or a drive letter. Gates
    /// whether absolute paths from the agent are in scope at all.
    read_allows_absolute: bool,
    write_allows_absolute: bool,
    net: NetPolicy,
    secrets: Vec<Regex>,
}

impl Policy {
    /// Compile every pattern once, up front, so malformed config fails fast at
    /// startup rather than halfway through a tool call.
    pub fn compile(config: &Config) -> Result<Self> {
        let fs = &config.filesystem;
        Ok(Self {
            tools: config.tool_rules()?,
            read_globs: build_globset(&fs.read)?,
            write_globs: build_globset(&fs.write)?,
            read_allows_absolute: fs.read.iter().any(|p| is_absolute(p)),
            write_allows_absolute: fs.write.iter().any(|p| is_absolute(p)),
            net: config.network.clone(),
            secrets: config.secret_rules()?,
        })
    }

    pub fn tools(&self) -> &ToolRules {
        &self.tools
    }

    /// Decide whether the agent may call a tool, by exposed name.
    pub fn may_call(&self, exposed_name: &str) -> Verdict {
        // Only tools inside the configured namespace are reachable, and only
        // if the allow/deny rules admit them. Checking both stops an agent
        // from reaching a tool the upstream exposes but the policy hides.
        let Some(upstream) = self.tools.upstream_name(exposed_name) else {
            return Verdict::Deny;
        };
        if !self.tools.admits(exposed_name) && !self.tools.admits(upstream) {
            return Verdict::Deny;
        }
        Verdict::Allow
    }

    /// Map an exposed tool name back to the upstream name, rejecting names
    /// outside the configured namespace.
    pub fn resolve_tool(&self, exposed_name: &str) -> Option<String> {
        let upstream = self.tools.upstream_name(exposed_name)?;
        if self.tools.namespace().is_some() && !self.tools.admits(exposed_name) {
            return None;
        }
        Some(upstream.to_string())
    }

    /// Check a path read against the filesystem policy.
    ///
    /// Part of the public policy surface: callers embedding mcp-sandman as a
    /// library use this to validate a path before offering it to an agent.
    #[allow(dead_code)]
    pub fn may_read(&self, path: &str) -> Verdict {
        if is_absolute(path) && !self.read_allows_absolute {
            return Verdict::Deny;
        }
        globset_check(&self.read_globs, path)
    }

    /// Check a path write against the filesystem policy.
    #[allow(dead_code)]
    pub fn may_write(&self, path: &str) -> Verdict {
        if self.write_globs.is_empty() {
            return Verdict::Deny;
        }
        if is_absolute(path) && !self.write_allows_absolute {
            return Verdict::Deny;
        }
        globset_check(&self.write_globs, path)
    }

    /// Check whether the sandbox may reach a host at all.
    #[allow(dead_code)]
    pub fn may_reach(&self, host: &str, port: u16) -> Verdict {
        if self.net.is_offline() {
            return Verdict::Deny;
        }
        if self.net.block_dns {
            // Nothing may be resolved by the sandbox itself; an IP literal
            // with an explicit allow rule is the only way through.
            let is_ip = host.parse::<std::net::IpAddr>().is_ok();
            if !is_ip {
                return Verdict::Deny;
            }
        }
        let denied = host_matches(&self.net.deny_hosts, host);
        if denied {
            return Verdict::Deny;
        }
        if !self.net.allow_ports.is_empty() && !self.net.allow_ports.contains(&port) {
            return Verdict::Deny;
        }
        if host_matches(&self.net.allow_hosts, host) {
            Verdict::Allow
        } else {
            Verdict::Deny
        }
    }

    /// Inspect tool arguments for filesystem and network access the policy forbids.
    ///
    /// This is a best-effort check over argument *strings*: it catches the
    /// ordinary cases (a `path` argument pointing at `/etc/shadow`, a `url`
    /// argument pointing at an unlisted host) without pretending to model what
    /// the server will actually do with them. A determined server that builds a
    /// path at runtime still needs kernel-level isolation — that is what the
    /// `container` isolation mode is for.
    pub fn check_resources(&self, args: &Value) -> Option<String> {
        fn walk(
            value: &Value,
            key_hint: &str,
            read: &GlobSet,
            write: &GlobSet,
            net: &NetPolicy,
            read_abs: bool,
            write_abs: bool,
        ) -> Option<String> {
            match value {
                Value::Object(map) => map
                    .iter()
                    .find_map(|(k, v)| walk(v, k, read, write, net, read_abs, write_abs)),
                Value::Array(items) => items
                    .iter()
                    .find_map(|i| walk(i, key_hint, read, write, net, read_abs, write_abs)),
                Value::String(s) => {
                    if is_http_url(s) {
                        return check_url(s, net);
                    }
                    // Only treat a string as a path when the argument name says
                    // so, or when it looks like one. Judging every string as a
                    // path would deny ordinary text arguments.
                    if looks_like_path(key_hint, s) {
                        // `**` in a globset matches across `/`, so a policy of
                        // `["**"]` would otherwise permit `/etc/shadow`. Treat
                        // an absolute path as in-scope only when the policy
                        // itself names absolute paths, which is what an operator
                        // writing `["/**"]` or `["C:/**"]` has clearly asked for.
                        let writing = is_write_key(key_hint);
                        let abs_ok = if writing { write_abs } else { read_abs };
                        if is_absolute(s) && !abs_ok {
                            return Some(format!(
                                "access to the absolute path `{s}` is not permitted; \
                                 this policy only allows paths relative to the working directory"
                            ));
                        }
                        let globset = if writing { write } else { read };
                        if !matches!(globset_check(globset, s), Verdict::Allow) {
                            return Some(format!(
                                "filesystem access to `{s}` is not permitted by this sandbox"
                            ));
                        }
                    }
                    None
                }
                _ => None,
            }
        }

        walk(
            args,
            "",
            &self.read_globs,
            &self.write_globs,
            &self.net,
            self.read_allows_absolute,
            self.write_allows_absolute,
        )
    }

    /// Scan tool arguments for anything that looks like a secret.
    ///
    /// This is a coarse tripwire, not a DLP. It exists because the common
    /// failure mode is an agent being talked into reading a credential file
    /// and posting it somewhere. With no `secret_patterns` configured this is
    /// a no-op, so it costs nothing until someone opts in.
    pub fn scan_for_secrets(&self, args: &Value) -> Option<String> {
        fn walk(value: &Value, patterns: &[Regex]) -> Option<String> {
            match value {
                Value::String(s) => patterns
                    .iter()
                    .find(|p| p.is_match(s))
                    .map(|p| format!("argument matched secret pattern /{p}/")),
                Value::Array(items) => items.iter().find_map(|i| walk(i, patterns)),
                Value::Object(map) => map.values().find_map(|v| walk(v, patterns)),
                _ => None,
            }
        }
        if self.secrets.is_empty() {
            return None;
        }
        walk(args, &self.secrets)
    }
}

/// Whether a path is absolute: a leading `/`, or a Windows drive letter.
///
/// `./x` is deliberately not absolute — it names a file relative to where the
/// sandbox runs, which is exactly what a relative policy glob should match.
fn is_absolute(path: &str) -> bool {
    let unified = path.replace('\\', "/");
    if unified.starts_with('/') {
        return true;
    }
    let bytes = unified.as_bytes();
    // `C:` or `C:/...` — a drive letter followed by a colon.
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Normalize a path so policy globs are written the way a human thinks.
///
/// `./scratch/a.txt`, `scratch/a.txt` and `scratch\\a.txt` all name the same
/// file, and an operator should not have to guess which form the agent will
/// send. Separators become `/` and a leading `./` is dropped.
///
/// A leading `/` is **not** stripped. Dropping it would turn `/etc/shadow`
/// into `etc/shadow`, which a `**` glob would then happily match — turning a
/// path restriction into no restriction at all. Absolute paths stay absolute,
/// and a policy that wants to allow them must say `/**` or `/` explicitly.
fn normalize_path(path: &str) -> String {
    let unified = path.replace('\\', "/");
    match unified.strip_prefix("./") {
        Some(rest) => rest.to_string(),
        None => unified,
    }
}

fn globset_check(globs: &GlobSet, path: &str) -> Verdict {
    if globs.is_empty() {
        return Verdict::Deny;
    }
    if globs.is_match(path) || globs.is_match(normalize_path(path)) {
        Verdict::Allow
    } else {
        Verdict::Deny
    }
}

/// Whether a string is an `http`/`https` URL we can extract a host from.
fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Whether an argument name suggests the value is a filesystem path.
fn looks_like_path(key_hint: &str, value: &str) -> bool {
    let key = key_hint.to_ascii_lowercase();
    let named = [
        "path",
        "file",
        "dir",
        "directory",
        "folder",
        "src",
        "dest",
        "target",
    ]
    .iter()
    .any(|k| key.contains(k));
    let looks_absolute = value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || (value.len() > 2 && value.as_bytes()[1] == b':'); // C:\ or D:\
    named || looks_absolute
}

/// Whether an argument name suggests the value is a write destination.
fn is_write_key(key_hint: &str) -> bool {
    let key = key_hint.to_ascii_lowercase();
    [
        "write", "dest", "output", "save", "create", "target", "upload",
    ]
    .iter()
    .any(|k| key.contains(k))
}

/// Extract host and port from an http(s) URL and check the network policy.
fn check_url(url: &str, net: &NetPolicy) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // Drop userinfo, path, query and fragment.
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h, p.parse().ok())
        }
        _ => (host_port, None),
    };
    let default_port = if url.starts_with("https://") { 443 } else { 80 };
    let port = port.unwrap_or(default_port);

    if !host_matches(&net.allow_hosts, host) {
        return Some(format!(
            "network access to `{host}` is not permitted by this sandbox"
        ));
    }
    if !net.allow_ports.is_empty() && !net.allow_ports.contains(&port) {
        return Some(format!(
            "network access to port {port} is not permitted by this sandbox"
        ));
    }
    None
}

/// Build a [`GlobSet`], treating an empty pattern list as an empty set.
fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern)
                .map_err(|e| crate::error::config_err(format!("invalid glob `{pattern}`: {e}")))?,
        );
    }
    builder
        .build()
        .map_err(|e| crate::error::config_err(e.to_string()))
}

/// Match a host against a list of patterns, supporting `*.example.com`.
fn host_matches(patterns: &[String], host: &str) -> bool {
    patterns.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host.eq_ignore_ascii_case(pattern)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NetPolicy, ToolPolicy};

    fn config_with(tools: ToolPolicy, net: NetPolicy) -> Config {
        Config {
            tools,
            network: net,
            ..Default::default()
        }
    }

    #[test]
    fn deny_covers_a_tool_the_policy_hides() {
        let cfg = config_with(
            ToolPolicy {
                allow: vec!["read_*".into()],
                ..Default::default()
            },
            NetPolicy::default(),
        );
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(policy.may_call("read_file"), Verdict::Allow);
        assert_eq!(policy.may_call("write_file"), Verdict::Deny);
    }

    #[test]
    fn write_is_denied_when_no_write_globs_configured() {
        let policy = Policy::compile(&Config::default()).unwrap();
        assert_eq!(policy.may_read("tmp/x"), Verdict::Allow);
        assert_eq!(policy.may_write("tmp/x"), Verdict::Deny);
    }

    #[test]
    fn write_globs_open_only_the_listed_paths() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["**".into()],
                write: vec!["workspace/**".into()],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(policy.may_write("workspace/a.txt"), Verdict::Allow);
        assert_eq!(policy.may_write("etc/passwd"), Verdict::Deny);
    }

    #[test]
    fn empty_network_policy_is_offline() {
        let policy = Policy::compile(&Config::default()).unwrap();
        assert_eq!(policy.may_reach("example.com", 443), Verdict::Deny);
    }

    #[test]
    fn wildcard_subdomains_match() {
        let net = NetPolicy {
            allow_hosts: vec!["*.example.com".into()],
            ..Default::default()
        };
        let cfg = config_with(ToolPolicy::default(), net);
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(policy.may_reach("api.example.com", 443), Verdict::Allow);
        assert_eq!(policy.may_reach("example.com", 443), Verdict::Allow);
        assert_eq!(policy.may_reach("evil.com", 443), Verdict::Deny);
    }

    #[test]
    fn port_restriction_applies() {
        let net = NetPolicy {
            allow_hosts: vec!["example.com".into()],
            allow_ports: vec![443],
            ..Default::default()
        };
        let cfg = config_with(ToolPolicy::default(), net);
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(policy.may_reach("example.com", 443), Verdict::Allow);
        assert_eq!(policy.may_reach("example.com", 22), Verdict::Deny);
    }

    #[test]
    fn deny_hosts_win_over_allow() {
        let net = NetPolicy {
            allow_hosts: vec!["*.example.com".into()],
            deny_hosts: vec!["api.example.com".into()],
            ..Default::default()
        };
        let cfg = config_with(ToolPolicy::default(), net);
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(policy.may_reach("api.example.com", 443), Verdict::Deny);
        assert_eq!(policy.may_reach("www.example.com", 443), Verdict::Allow);
    }

    #[test]
    fn block_dns_rejects_hostnames() {
        let net = NetPolicy {
            allow_hosts: vec!["example.com".into()],
            block_dns: true,
            ..Default::default()
        };
        let cfg = config_with(ToolPolicy::default(), net);
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(policy.may_reach("example.com", 443), Verdict::Deny);
    }

    #[test]
    fn resource_check_allows_paths_within_read_glob() {
        let policy = Policy::compile(&Config::default()).unwrap();
        let args = serde_json::json!({"path": "workspace/a.txt"});
        assert_eq!(policy.check_resources(&args), None);
    }

    #[test]
    fn resource_check_denies_path_outside_read_glob() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["/workspace/**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        let args = serde_json::json!({"path": "/etc/shadow"});
        let reason = policy.check_resources(&args).expect("should deny");
        assert!(reason.contains("/etc/shadow"), "{reason}");
    }

    #[test]
    fn resource_check_denies_disallowed_host() {
        let policy = Policy::compile(&Config::default()).unwrap();
        let args = serde_json::json!({"url": "https://evil.example.org/x"});
        let reason = policy.check_resources(&args).expect("should deny");
        assert!(reason.contains("evil.example.org"), "{reason}");
    }

    #[test]
    fn resource_check_allows_configured_host() {
        let net = NetPolicy {
            allow_hosts: vec!["api.github.com".into()],
            ..Default::default()
        };
        let cfg = config_with(ToolPolicy::default(), net);
        let policy = Policy::compile(&cfg).unwrap();
        let args = serde_json::json!({"url": "https://api.github.com/repos"});
        assert_eq!(policy.check_resources(&args), None);
    }

    #[test]
    fn write_dest_uses_the_write_policy() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["**".into()],
                write: vec!["workspace/**".into()],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        // readable, but not writable outside workspace
        assert_eq!(
            policy.check_resources(&serde_json::json!({"path": "etc/hosts"})),
            None
        );
        let denial = policy
            .check_resources(&serde_json::json!({"dest_path": "etc/crontab"}))
            .expect("write outside workspace should deny");
        assert!(denial.contains("etc/crontab"), "{denial}");
    }

    #[test]
    fn ordinary_text_arguments_are_not_treated_as_paths() {
        let policy = Policy::compile(&Config::default()).unwrap();
        let args = serde_json::json!({"message": "hello", "count": 3});
        assert_eq!(policy.check_resources(&args), None);
    }

    #[test]
    fn absolute_paths_are_not_made_relative() {
        // Regression guard: if normalization ever stripped a leading `/`, this
        // policy (`**` only) would start allowing `/etc/shadow`.
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        let denial = policy
            .check_resources(&serde_json::json!({"path": "/etc/shadow"}))
            .expect("an absolute path must not match a relative-only glob");
        assert!(denial.contains("/etc/shadow"), "{denial}");
    }

    #[test]
    fn dot_slash_prefix_is_ignored() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["src/**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(
            policy.check_resources(&serde_json::json!({"path": "./src/main.rs"})),
            None,
            "`./src/main.rs` and `src/main.rs` name the same file"
        );
    }

    #[test]
    fn windows_separators_normalize() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["scratch/**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(
            policy.check_resources(&serde_json::json!({"path": "scratch\\note.txt"})),
            None
        );
    }

    #[test]
    fn url_ports_are_respected() {
        let net = NetPolicy {
            allow_hosts: vec!["example.com".into()],
            allow_ports: vec![443],
            ..Default::default()
        };
        let cfg = config_with(ToolPolicy::default(), net);
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(
            policy.check_resources(&serde_json::json!({"url": "https://example.com/a"})),
            None
        );
        let reason = policy
            .check_resources(&serde_json::json!({"url": "http://example.com/a"}))
            .expect("port 80 is not allowed");
        assert!(reason.contains("port 80"), "{reason}");
    }

    #[test]
    fn nested_arguments_are_inspected() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["src/**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        let args = serde_json::json!({"payload": {"inner": {"path": "/etc/passwd"}}});
        assert!(policy.check_resources(&args).is_some());
    }

    #[test]
    fn absolute_paths_are_allowed_when_the_policy_says_so() {
        // The escape hatch: an operator who writes `/**` has asked for absolute
        // paths, and the sandbox must not silently refuse to honour that.
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["/tmp/**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(
            policy.check_resources(&serde_json::json!({"path": "/tmp/notes.txt"})),
            None
        );
    }

    #[test]
    fn windows_absolute_paths_are_recognised() {
        let cfg = Config {
            filesystem: crate::config::FsPolicy {
                read: vec!["**".into()],
                write: vec![],
                deny_by_default: true,
            },
            ..Default::default()
        };
        let policy = Policy::compile(&cfg).unwrap();
        assert_eq!(
            policy.may_read("C:\\Windows\\win.ini"),
            Verdict::Deny,
            "a drive-letter path must not slip past a relative-only policy"
        );
    }
}
