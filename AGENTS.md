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

# Console
- `src/console.rs` is the docked terminal: `egui_term` over `alacritty_terminal`, in an egui side or bottom panel. The implementation sits behind the `Terminal` trait, so a different renderer swaps in without touching docking, focus or the header; `Console::with_terminal` is how the tests drive it without a pty.
- Console fonts are a chain, not one font: egui's monospace family is ordered, and `install_symbol_fallbacks` appends per-platform candidates at lowest priority so Hack keeps the metrics. Always validate font bytes with `ab_glyph` before handing them to egui, which panics on what it cannot parse.
- Key routing turns on one branch in `egui_app.rs` (`WindowEvent::KeyboardInput`): keys go to the remote desktop unless the console has focus, in which case they fall through to egui. `ModifiersChanged` follows the same rule.
- The console panel is declared before the central panel, so `desktop_rect` shrinks on its own and both the session resize and the pointer hit testing follow without knowing about it.
- The keyboard grab is driven through `sync_shortcut_capture`, which folds window focus and console focus into the one `set_focused` lever.
- `console::tests::runs_a_shell_and_notices_it_leaving` spawns a real pty, so the backend wiring is covered.

# Documentation
Only document if there is a good reason to do so (big plan). Don't document minor changes or tasks
that are not necessary to read about in the future.
