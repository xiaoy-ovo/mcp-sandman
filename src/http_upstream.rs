//! Streamable HTTP transport for upstreams that are not child processes.
//!
//! MCP over HTTP is a single endpoint that accepts a JSON-RPC request as a POST
//! body and answers with either a JSON body or an SSE stream, depending on the
//! server. We handle both: if the response is `text/event-stream`, we read
//! events until one carries our result.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::error::{Result, SandmanError};
use crate::protocol::{Message, initialize_params, methods};
use crate::upstream::ToolInfo;

/// Convert a transport-level reqwest failure into a sandbox error.
///
/// Kept separate so the `?` sites stay readable and every failure gets the
/// same "this is a network problem, not a protocol one" framing.
fn http_err(e: reqwest::Error) -> SandmanError {
    SandmanError::Config(format!("HTTP upstream request failed: {e}"))
}

/// An MCP server reached over Streamable HTTP.
pub struct HttpConnection {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    next_id: AtomicU64,
}

impl HttpConnection {
    /// Build a connection. No network traffic happens here; the handshake in
    /// [`Self::handshake`] does that.
    pub fn new(url: String, headers: BTreeMap<String, String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            url,
            headers,
            next_id: AtomicU64::new(1),
        }
    }

    fn take_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Perform the MCP initialize handshake over HTTP.
    async fn handshake(&self, timeout_ms: u64) -> Result<()> {
        let id = self.take_id();
        let params = initialize_params("mcp-sandman");
        let response = self
            .post(&Message::request(json!(id), methods::INITIALIZE, params), timeout_ms)
            .await?;

        if let Some(error) = response.error {
            return Err(SandmanError::Config(format!(
                "upstream rejected initialize: {}",
                error.message
            )));
        }

        // The server expects this notification, and answers with 202 by design,
        // so a non-success status here is not worth failing on.
        if let Err(e) = self
            .notify(methods::INITIALIZED, json!({}))
            .await
        {
            tracing::debug!(error = %e, "upstream did not acknowledge the initialized notification");
        }
        Ok(())
    }

    /// POST one JSON-RPC frame and return the matching result.
    async fn request(&self, method: &str, params: Value, timeout_ms: u64) -> Result<Value> {
        let id = self.take_id();
        let message = Message::request(json!(id), method, params);
        let frame = self.post(&message, timeout_ms).await?;

        if let Some(error) = frame.error {
            return Err(SandmanError::PolicyViolation(error.message));
        }
        Ok(frame.result.unwrap_or(Value::Null))
    }

    /// Send one frame and unwrap the response envelope.
    async fn post(&self, message: &Message, timeout_ms: u64) -> Result<Message> {
        let body = serde_json::to_string(message)?;
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .body(body);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }

        let response = request.send().await.map_err(|e| {
            SandmanError::Config(format!("cannot reach HTTP upstream at {}: {e}", self.url))
        })?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(SandmanError::Config(format!(
                "HTTP upstream returned {status}: {}",
                detail.chars().take(300).collect::<String>()
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        if content_type.starts_with("text/event-stream") {
            self.read_sse(response).await
        } else {
            let text = response.text().await.map_err(http_err)?;
            serde_json::from_str(&text).map_err(|e| {
                SandmanError::Config(format!("upstream returned unparsable JSON: {e}"))
            })
        }
    }

    /// Read an SSE stream until a `data:` frame carries a JSON-RPC message.
    ///
    /// Servers stream the response body as SSE and may send comments or
    /// keep-alives first; only `data:` lines matter here.
    async fn read_sse(&self, response: reqwest::Response) -> Result<Message> {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                SandmanError::Config(format!("SSE stream failed: {e}"))
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(nl) = buffer.find('\n') {
                let line = buffer[..nl].trim_end_matches('\r').to_string();
                buffer.drain(..=nl);

                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }
                if let Ok(message) = serde_json::from_str::<Message>(payload) {
                    return Ok(message);
                }
            }
        }
        Err(SandmanError::UpstreamExited)
    }

    /// Send a notification, which by definition gets no body back.
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = serde_json::to_string(&Message::notification(method, params))?;
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        request.send().await.map_err(http_err)?;
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let result = self.request(methods::TOOLS_LIST, json!({}), 30_000).await?;
        Ok(crate::upstream::parse_tool_list(&result))
    }

    pub async fn call_tool(&self, name: &str, args: &Value, timeout_ms: u64) -> Result<Value> {
        self.request(
            methods::TOOLS_CALL,
            json!({ "name": name, "arguments": args }),
            timeout_ms,
        )
        .await
    }

    pub async fn forward(&self, method: &str, params: &Value, timeout_ms: u64) -> Result<Value> {
        self.request(method, params.clone(), timeout_ms).await
    }

    pub async fn shutdown(&self) {}
}

impl HttpConnection {
    /// Connect and handshake in one step, for the same interface stdio gets.
    pub async fn connect(url: String, headers: BTreeMap<String, String>, timeout_ms: u64) -> Result<Self> {
        let conn = Self::new(url, headers);
        conn.handshake(timeout_ms).await?;
        Ok(conn)
    }
}