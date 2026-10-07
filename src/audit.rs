//! Append-only audit trail of every tool call the sandbox decides on.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// One line in the audit trail.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub tool: String,
    /// `allow`, `deny`, or `error`.
    pub decision: String,
    /// Present when the sandbox refused the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Argument names only — never argument values, which may hold secrets.
    ///
    /// `default` pairs with the skip below: a reader must accept entries this
    /// build wrote with the field omitted, so the two have to agree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_names: Vec<String>,
    pub duration_ms: u64,
}

/// Appends audit entries to a newline-delimited JSON file.
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    /// Open the audit log for appending, creating parent directories.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // Touch the file so a misconfigured path fails at startup rather than
        // on the first tool call an operator is relying on to be recorded.
        OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Record one decision.
    ///
    /// Logging must never take down the proxy, so failures are reported to the
    /// operator's log and otherwise swallowed.
    pub fn record(&self, entry: &AuditEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(line) => line,
            Err(e) => {
                tracing::error!(error = %e, "could not serialize audit entry");
                return;
            }
        };
        if let Err(e) = self.append(&line) {
            tracing::error!(error = %e, path = %self.path.display(), "could not write audit entry");
        }
    }

    fn append(&self, line: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }
}

/// Build an audit entry, collecting argument *names* but never values.
pub fn entry(
    tool: &str,
    decision: &str,
    reason: Option<String>,
    args: &Value,
    duration_ms: u64,
) -> AuditEntry {
    AuditEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        tool: tool.to_string(),
        decision: decision.to_string(),
        reason,
        arg_names: arg_names(args),
        duration_ms,
    }
}

/// Extract argument names from a tool-call params object.
///
/// Only the keys are recorded. Values can contain credentials or source code,
/// and an audit log that stores them is a liability rather than a control.
fn arg_names(args: &Value) -> Vec<String> {
    match args.get("arguments") {
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_argument_names_not_values() {
        let args = json!({"arguments": {"path": "/etc/shadow", "token": "sk-secret"}});
        let names = arg_names(&args);
        assert_eq!(names, vec!["path", "token"]);
        // The value must not survive into the names list.
        assert!(!names.iter().any(|n| n.contains("secret")));
    }

    #[test]
    fn missing_arguments_yields_no_names() {
        assert!(arg_names(&json!({})).is_empty());
    }

    #[test]
    fn writes_one_json_object_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path).unwrap();
        log.record(&entry("a", "allow", None, &json!({}), 1));
        log.record(&entry("b", "deny", Some("nope".into()), &json!({}), 2));

        let text = std::fs::read_to_string(&path).unwrap();
        let count = text.split('\n').filter(|l| !l.is_empty()).count();
        assert_eq!(count, 2);
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            serde_json::from_str::<AuditEntry>(line).expect("each line is valid json");
        }
    }

    #[test]
    fn creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/audit.log");
        AuditLog::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn reason_is_omitted_when_absent() {
        let e = entry("a", "allow", None, &json!({}), 1);
        let text = serde_json::to_string(&e).unwrap();
        assert!(!text.contains("reason"), "{text}");
    }
}
