//! MCP mode: letting an agent see and drive a remote desktop.
//!
//! There are two ways in, and they differ only in who owns the RDP session:
//!
//! * **Headless** -- `irontsc mcp` opens a session of its own with no window and speaks MCP
//!   over stdio, the way an MCP client expects to start a server it owns.
//! * **In-session** -- the gear menu in a running window switches MCP on for the session the
//!   user is already watching. That one cannot use stdio, since the process is already
//!   running and the client has to connect to it, so it listens on loopback instead.
//!
//! Everything above the transport is shared: [`session::AgentSession`] is the desktop, and
//! [`server::McpServer`] is the tool surface over it.

pub mod keys;
pub mod screenshot;
pub mod server;
pub mod session;
pub mod vision;

pub use screenshot::Screenshot;
pub use server::{HttpServer, McpServer, serve_http, serve_stdio};
pub use session::{AgentError, AgentSession, FrameSummary, SharedFrame};
pub use vision::{Pixel, Region, RegionQuery};
