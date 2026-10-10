# Repository Guidelines

## Project Structure & Module Organization
- `src/` hosts the GTK client entry point (`main.rs`) and RDP channel glue modules such as `udp_transport.rs` and `gfx_channel.rs`.
- `crates/` contains vendor-pinned IronRDP crates; keep cross-crate changes atomic.
- `samples/` stores captured keylogs/pcaps for reproducing regressions.
- `vendor/` holds upstream protocol specs and scripts used for reference; avoid editing without updating generated artifacts.
- `target/` is Cargo's build output and must not be committed.

## Build, Test, and Development Commands
- `cargo build --release` produces the optimized client at `target/release/irontsc`.

## Testing Guidelines
- Use samples/irontsc-w11.pcap (with key irontsc-w11.key) and samples/irontsc-w11.log for checking protocol output.
- Don't redirect to another log. There will be a log file created with trace level details.
- There is a reference capture called samples/working-rdp.pcap with key from a working Windows RDP mstsc -> Windows 11 in UDP mode

# UDP mode
- When in UDP mode there is a reliable mode that uses TLS and another one that uses DTLS. On local networks it defaults to reliable mode,
so that's currently a priority to get fixed.

# Inspecting captures
tshark -r samples/working-rdp.pcapng -Y "rdpudp" is the correct way to analyze UDP packets

# RDP Specs
vendor/rdp-specs-md contains the different RDP specs in markdown format

# MCP mode
- `src/agent/` is the agent-facing side: `session.rs` is the desktop (framebuffer mirror plus input), `server.rs` the MCP tool surface, `vision.rs` the pixel and flat-rectangle queries, `keys.rs` chord parsing.
- Both modes share `AgentSession`. Headless (`irontsc mcp`) owns its RDP session and speaks stdio; the island's gear toggle attaches to the window's session and serves loopback HTTP.
- `tests/mcp_http.rs` drives the real HTTP surface against a synthetic desktop, so the rmcp wiring is covered without an RDP server.
- Key names map to `winit::keyboard::KeyCode` and then through `egui_scancode::scancode_for`, so there is one scancode table rather than two that can drift.

# Documentation
Only document if there is a good reason to do so (big plan). Don't document minor changes or tasks
that are not necessary to read about in the future.
