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
use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};

/// A connected session, with the id the server handed out at initialise.
///
/// One connection per request rather than a pool: this is a handful of loopback requests from
/// a command that then exits, and the transport answers each POST with a stream it closes.
pub struct Client {
    host: String,
    port: u16,
    target: String,
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
        let uri: hyper::Uri = url
            .parse()
            .with_context(|| format!("`{url}` is not a URL"))?;
        if uri.scheme_str() != Some("http") {
            anyhow::bail!("only plain http URLs are supported; the register hands out loopback");
        }

        let client = Self {
            host: uri
                .host()
                .ok_or_else(|| anyhow!("`{url}` has no host"))?
                .to_owned(),
            port: uri.port_u16().unwrap_or(80),
            target: uri
                .path_and_query()
                .map(|path| path.as_str().to_owned())
                .unwrap_or_else(|| "/".to_owned()),
            session_id: None,
            next_id: std::cell::Cell::new(1),
        };

        let (_initialised, session_id) = client
            .post(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "irontsc", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;

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

        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (status, session_id, body) = self
            .send(&payload.to_string())
            .await
            .with_context(|| format!("`{method}` could not reach the session"))?;

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
        let payload = json!({ "jsonrpc": "2.0", "method": method });
        self.send(&payload.to_string())
            .await
            .with_context(|| format!("`{method}` could not reach the session"))?;
        Ok(())
    }

    /// The HTTP itself: one connection, one POST, the whole reply read back.
    async fn send(&self, body: &str) -> anyhow::Result<(hyper::StatusCode, Option<String>, String)> {
        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .with_context(|| format!("nothing is listening on {}:{}", self.host, self.port))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        // The connection has to be driven while the request is in flight, and is finished with
        // once the reply has been read.
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut request = hyper::Request::post(&self.target)
            .header("host", format!("{}:{}", self.host, self.port))
            .header("content-type", "application/json")
            // The transport may answer either way, and says which in its content type.
            .header("accept", "application/json, text/event-stream");
        if let Some(session_id) = &self.session_id {
            request = request.header("mcp-session-id", session_id);
        }

        let response = sender
            .send_request(request.body(Full::new(Bytes::from(body.to_owned())))?)
            .await?;

        let status = response.status();
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let collected = response.into_body().collect().await?.to_bytes();

        Ok((
            status,
            session_id,
            String::from_utf8_lossy(&collected).into_owned(),
        ))
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
