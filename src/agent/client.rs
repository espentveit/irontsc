//! The other end of [`super::server`]: enough MCP client to drive a session from the CLI.
//!
//! `irontsc sessions` can say what is running, which is only half an answer -- the other half
//! is being able to do something with one without an agent in the loop. So this speaks the
//! same streamable-HTTP transport the server does, which is a POST per request whose reply
//! comes back as one server-sent event.
//!
//! It is deliberately small: initialise, list the tools, call one. Everything else an MCP
//! client can do belongs to a real client.

use anyhow::{Context as _, anyhow};
use serde_json::{Value, json};

/// A connected session, with the id the server handed out at initialise.
pub struct Client {
    http: reqwest::Client,
    url: String,
    session_id: Option<String>,
    next_id: std::cell::Cell<u64>,
}

/// One tool, as the server describes it.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

impl Client {
    /// Opens a session against the URL from the register.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client = Self {
            http: reqwest::Client::builder()
                .build()
                .context("could not build an HTTP client")?,
            url: url.to_owned(),
            session_id: None,
            next_id: std::cell::Cell::new(1),
        };

        let (response, session_id) = client
            .post(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "irontsc", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        let _ = response;

        let mut client = client;
        client.session_id = session_id;

        // The server is within its rights to reject everything else until this arrives.
        client.notify("notifications/initialized").await?;
        Ok(client)
    }

    /// Every tool the session offers, in the order it lists them.
    pub async fn tools(&self) -> anyhow::Result<Vec<ToolInfo>> {
        let (result, _) = self.post("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("the session listed no tools"))?;

        Ok(tools
            .iter()
            .map(|tool| ToolInfo {
                name: tool
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect())
    }

    /// Calls one tool and hands back its result untouched.
    pub async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<Value> {
        let (result, _) = self
            .post("tools/call", json!({ "name": tool, "arguments": arguments }))
            .await?;
        Ok(result)
    }

    /// One request, and the `result` from the event that comes back.
    async fn post(&self, method: &str, params: Value) -> anyhow::Result<(Value, Option<String>)> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);

        let mut request = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            // The transport may answer either way, and says which in its content type.
            .header("accept", "application/json, text/event-stream")
            .json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        if let Some(session_id) = &self.session_id {
            request = request.header("mcp-session-id", session_id);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("`{method}` could not reach the session"))?;

        let status = response.status();
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.context("unreadable reply")?;

        if !status.is_success() {
            let detail = body.trim();
            return Err(anyhow!("the session answered {status}: {detail}"));
        }

        let message = decode(&body)
            .ok_or_else(|| anyhow!("`{method}` came back in a shape this client cannot read"))?;
        if let Some(error) = message.get("error") {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            return Err(anyhow!("`{method}` was refused: {detail}"));
        }

        let result = message
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("`{method}` came back without a result"))?;
        Ok((result, session_id))
    }

    /// A notification, which by definition has no reply to wait for.
    async fn notify(&self, method: &str) -> anyhow::Result<()> {
        let mut request = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&json!({ "jsonrpc": "2.0", "method": method }));
        if let Some(session_id) = &self.session_id {
            request = request.header("mcp-session-id", session_id);
        }
        request
            .send()
            .await
            .with_context(|| format!("`{method}` could not reach the session"))?;
        Ok(())
    }
}

/// Pulls the JSON-RPC message out of a reply, which is either JSON itself or a stream of
/// server-sent events with the message on a `data:` line.
fn decode(body: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(body)
        && value.is_object()
    {
        return Some(value);
    }

    // Last one wins: the reply to the request is the last message on the stream, after any
    // progress notifications the server sent while it worked.
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .rfind(|value| value.get("result").is_some() || value.get("error").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_message_off_an_event_stream() {
        let body = "data: \nid: 0\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let message = decode(body).expect("the stream carries a message");
        assert_eq!(message["result"]["ok"], json!(true));
    }

    #[test]
    fn reads_a_plain_json_reply() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}";
        let message = decode(body).expect("the reply is a message");
        assert_eq!(message["result"]["ok"], json!(true));
    }

    #[test]
    fn takes_the_reply_rather_than_a_notification_before_it() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"second\":true}}\n\n",
        );
        let message = decode(body).expect("the stream carries a message");
        assert_eq!(message["result"]["second"], json!(true));
    }
}
