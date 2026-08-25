//! Drives the in-session MCP server over its own HTTP surface.
//!
//! The tool bodies are unit-tested next to the code they live in; what this covers is the
//! part that cannot be checked by reading -- that the rmcp wiring, the token gate and the
//! streamable-HTTP transport actually answer an MCP client. The desktop is synthetic, so no
//! RDP server is involved.

use std::sync::Arc;

use irontsc::agent::{AgentSession, SharedFrame};
use irontsc::rdp::RdpInputEvent;

/// A fake desktop: a 64x32 BGRA surface with one distinguishable pixel.
fn fake_desktop() -> Arc<SharedFrame> {
    let frame = Arc::new(SharedFrame::new());
    let width = std::num::NonZeroU16::new(64).expect("non-zero");
    let height = std::num::NonZeroU16::new(32).expect("non-zero");

    let mut bgra = vec![0u8; 64 * 32 * 4];
    // Blue, green, red, alpha -- an obviously non-black pixel at the origin.
    bgra[0..4].copy_from_slice(&[255, 128, 64, 255]);

    frame.apply_image(&bgra, width, height, None);
    frame
}

struct Harness {
    url: String,
    client: reqwest::Client,
    session_id: Option<String>,
    /// Held so the session's input channel stays open for the duration of the test.
    _input: tokio::sync::mpsc::UnboundedReceiver<RdpInputEvent>,
    _server: irontsc::agent::HttpServer,
}

impl Harness {
    async fn start() -> Self {
        let frame = fake_desktop();
        let (sender, receiver) = RdpInputEvent::create_channel();
        let session = AgentSession::attach(
            frame,
            sender,
            irontsc::agent::KeyboardLayout::resolve(""),
        );

        let server = irontsc::agent::serve_http(session, 0, None)
            .await
            .expect("the MCP server binds");

        Self {
            url: server.url(),
            client: reqwest::Client::new(),
            session_id: None,
            _input: receiver,
            _server: server,
        }
    }

    /// Sends one JSON-RPC message and returns the parsed reply, whether it came back as JSON
    /// or as a single SSE event.
    async fn call(&mut self, body: serde_json::Value) -> serde_json::Value {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");

        if let Some(id) = self.session_id.as_ref() {
            request = request.header("mcp-session-id", id);
        }

        let response = request.json(&body).send().await.expect("the server answers");
        assert!(
            response.status().is_success(),
            "unexpected status {}",
            response.status()
        );

        if let Some(id) = response.headers().get("mcp-session-id") {
            self.session_id = Some(id.to_str().expect("ascii session id").to_owned());
        }

        let text = response.text().await.expect("a body");
        parse_reply(&text)
    }
}

/// The transport answers either `application/json` or an SSE stream; both carry the same
/// JSON-RPC object.
fn parse_reply(body: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        return value;
    }
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                return value;
            }
        }
    }
    panic!("could not parse an MCP reply out of: {body}");
}

fn initialize_request() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "irontsc-test", "version": "0" }
        }
    })
}

#[tokio::test]
async fn refuses_a_request_without_the_token() {
    let harness = Harness::start().await;
    // Strip the query string, and with it the token.
    let bare = harness
        .url
        .split('?')
        .next()
        .expect("a url before the query")
        .to_owned();

    let response = harness
        .client
        .post(&bare)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_request())
        .send()
        .await
        .expect("the server answers");

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refuses_a_request_with_the_wrong_token() {
    let harness = Harness::start().await;
    let bare = harness.url.split('?').next().expect("a url").to_owned();

    let response = harness
        .client
        .post(format!("{bare}?t=not-the-token"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_request())
        .send()
        .await
        .expect("the server answers");

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn initializes_and_lists_its_tools() {
    let mut harness = Harness::start().await;

    let initialized = harness.call(initialize_request()).await;
    assert!(
        initialized["result"]["serverInfo"].is_object(),
        "expected a serverInfo in {initialized}"
    );

    let listed = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .await;

    let names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("a tools array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
        .collect();

    for expected in [
        "screenshot",
        "status",
        "click",
        "move_mouse",
        "drag",
        "scroll",
        "type_text",
        "key",
        "wait",
        "pixel",
        "find_regions",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "`{expected}` missing from {names:?}"
        );
    }
}

#[tokio::test]
async fn screenshots_the_desktop_as_a_png() {
    let mut harness = Harness::start().await;
    harness.call(initialize_request()).await;

    let called = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "screenshot",
                // No settling wait: the fake desktop is never going to change again.
                "arguments": { "settle_ms": 0 }
            }
        }))
        .await;

    let content = called["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("expected content in {called}"));

    let note = content
        .iter()
        .find(|block| block["type"] == "text")
        .expect("a text block");
    assert!(
        note["text"]
            .as_str()
            .expect("text")
            .contains("64x32"),
        "the note should state the desktop size: {note}"
    );

    let image = content
        .iter()
        .find(|block| block["type"] == "image")
        .expect("an image block");
    assert_eq!(image["mimeType"], "image/png");

    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(image["data"].as_str().expect("base64 data"))
        .expect("valid base64");
    assert_eq!(&decoded[1..4], b"PNG", "not a PNG: {:?}", &decoded[..8]);
}

#[tokio::test]
async fn screenshots_a_region_at_full_detail() {
    let mut harness = Harness::start().await;
    harness.call(initialize_request()).await;

    let called = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "screenshot",
                "arguments": { "settle_ms": 0, "x": 8, "y": 4, "width": 16, "height": 8 }
            }
        }))
        .await;

    let note = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a note in {called}"));

    // The desktop size still leads, so a model never loses the click coordinate space.
    assert!(note.contains("64x32"), "{note}");
    assert!(note.contains("region at (8, 4)"), "{note}");
    assert!(note.contains("16x8"), "{note}");
}

#[tokio::test]
async fn reads_a_pixel_by_coordinate() {
    let mut harness = Harness::start().await;
    harness.call(initialize_request()).await;

    let called = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "pixel", "arguments": { "x": 0, "y": 0 } }
        }))
        .await;

    let text = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected pixel text in {called}"));

    // The fake desktop's origin pixel is BGRA [255, 128, 64], i.e. rgb(64, 128, 255).
    assert!(text.contains("#4080ff"), "{text}");
    assert!(text.contains("rgb(64, 128, 255)"), "{text}");
}

#[tokio::test]
async fn refuses_half_a_coordinate_pair() {
    let mut harness = Harness::start().await;
    harness.call(initialize_request()).await;

    let called = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "pixel", "arguments": { "x": 3 } }
        }))
        .await;

    assert_eq!(called["result"]["isError"], true, "{called}");
}

#[tokio::test]
async fn finds_flat_regions() {
    let mut harness = Harness::start().await;
    harness.call(initialize_request()).await;

    let called = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": { "name": "find_regions", "arguments": {} }
        }))
        .await;

    let text = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected region text in {called}"));

    // The fake desktop is one big black block with a single odd pixel, so the block is found
    // and reported with a centre to click.
    assert!(text.contains("centre"), "{text}");
    assert!(text.contains("64x32"), "{text}");
}

#[tokio::test]
async fn reports_status_for_an_attached_session() {
    let mut harness = Harness::start().await;
    harness.call(initialize_request()).await;

    let called = harness
        .call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "status", "arguments": {} }
        }))
        .await;

    let text = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected status text in {called}"));

    assert!(text.contains("connected: true"), "{text}");
    assert!(text.contains("64x32"), "{text}");
    assert!(
        text.contains("attached to an IronTSC window"),
        "an attached session should say so: {text}"
    );
}
