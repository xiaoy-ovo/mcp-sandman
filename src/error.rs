//! Error types shared across the sandbox.

use thiserror::Error;

/// Anything that can go wrong while loading config or running the proxy.
#[derive(Debug, Error)]
pub enum SandmanError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("failed to parse TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error(
        "the upstream MCP server stopped responding mid-conversation \
         (it crashed or exited). Check its logs; `sandman doctor` will confirm \
         whether it can still start"
    )]
    UpstreamExited,

    #[error("upstream did not respond within {0}ms")]
    UpstreamTimeout(u64),

    #[error("tool `{0}` is not exposed by this sandbox")]
    UnknownTool(String),

    #[error("policy violation: {0}")]
    PolicyViolation(String),

    #[error("HTTP upstream support was not compiled in; rebuild with `--features http`")]
    HttpDisabled,
}

pub type Result<T> = std::result::Result<T, SandmanError>;

/// Shorthand for building a [`SandmanError::Config`].
pub fn config_err(msg: impl Into<String>) -> SandmanError {
    SandmanError::Config(msg.into())
}
