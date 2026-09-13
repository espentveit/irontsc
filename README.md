# IronTSC

IronTSC is an experimental native Remote Desktop Protocol client built in Rust
on [IronRDP](https://github.com/Devolutions/IronRDP). Its desktop UI uses egui,
winit, and glutin and is inspired by the Windows Remote Desktop client
(`mstsc`).

IronTSC supports saved `.rdp` connections, clipboard and audio redirection,
dynamic desktop resizing, RDP graphics codecs, TCP and UDP transports, a docked
terminal, and an MCP interface for agent-driven sessions.

## Download

GitHub Actions creates downloadable builds for pushes to `main`, pull requests,
manual runs, and version tags:

| Platform | Packages |
| --- | --- |
| Linux x86-64 | `.tar.gz` archive |
| Windows x64 | Portable ZIP and MSI installer |

Tagged commits such as `v0.1.0` are published as GitHub Releases with a
`SHA256SUMS` file. Builds from branches and pull requests are available in the
workflow run's **Artifacts** section.

The CI packages use the portable `gfx` feature set and omit H.264 and video
redirection, which require FFmpeg 8 development libraries. Build from source
with the default features to enable them.

## Build from source

Install a stable Rust toolchain with [rustup](https://rustup.rs/), then install
the platform's native development dependencies. See [BUILD.md](BUILD.md) for
the Debian/Ubuntu, Fedora, and Arch package lists.

Build the full client:

```sh
cargo build --locked --release
```

If FFmpeg 8 is unavailable, use the same feature set as the downloadable CI
builds:

```sh
cargo build --locked --release --no-default-features --features gfx
```

The executable is `target/release/irontsc` on Linux and
`target\release\irontsc.exe` on Windows.

## Usage

Open the connection dialog:

```sh
irontsc
```

Connect directly:

```sh
irontsc --computer server.example.com --username user --password secret --autologon
```

Open a saved connection:

```sh
irontsc connection.rdp
```

Run `irontsc --help` for all connection, folder sharing, camera, audio capture,
configuration, and MCP options. Only warnings and errors are logged by default.
Set `RUST_LOG` to opt into more detail, for example `RUST_LOG=debug irontsc`.

## MCP mode

IronTSC can expose a remote desktop through the Model Context Protocol. Enable
MCP from the session toolbar to let an agent join the visible session, or start
a headless stdio session:

```sh
irontsc mcp --computer server.example.com --username user --password secret
```

The MCP surface provides screenshots, pixel and region queries, mouse and
keyboard input, status, waiting, and disconnect controls.

## Development

Build the release client with:

```sh
cargo build --locked --release
```

Run the workspace tests with:

```sh
cargo test --workspace
```

Protocol captures and keys used for regression investigation live in
[`samples/`](samples/). Microsoft protocol documents used as implementation
references live in [`vendor/rdp-specs-md/`](vendor/rdp-specs-md/).

## License

MIT
