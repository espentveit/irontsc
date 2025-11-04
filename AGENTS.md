# Repository Guidelines

## Project Structure & Module Organization
- `src/` hosts the GTK client entry point (`main.rs`) and RDP channel glue modules such as `udp_transport.rs` and `gfx_channel.rs`.
- `crates/` contains vendor-pinned IronRDP crates; keep cross-crate changes atomic.
- `samples/` stores captured keylogs/pcaps for reproducing regressions.
- `vendor/` holds upstream protocol specs and scripts used for reference; avoid editing without updating generated artifacts.
- `target/` is Cargo's build output and must not be committed.

## Build, Test, and Development Commands
- `cargo build --release` produces the optimized client at `target/release/irontsc`.
- `./test.sh` orchestrates end-to-end testing. Needed for each run, because it resets the rdp server to a known state.

## Testing Guidelines
- Use samples/irontsc-w11.pcap (with key irontsc-w11.key) and samples/irontsc-w11.log) for checking output of test.sh
- There is a reference capture called samples/working-rdp.pcap with key from a working Windows RDP mstsc -> Windows 11 in UDP mode

# UDP mode
- When in UDP mode there is a reliable mode that uses TLS and another one that uses DTLS. On local networks it defaults to reliable mode,
so that's currently a priority to get fixed.

# Inspecting captures
tshark -r samples/working-rdp.pcapng -Y "rdpudp" is the correct way to analyze UDP packets

# RDP Specs
vendor/rdp-specs-md contains the different RDP specs in markdown format

# Documentation
Only document if there is a good reason to do so (big plan). Don't document minor changes or tasks
that are not necessary to read about in the future.