# IronTSC

IronTSC is an experimental native Remote Desktop Protocol client built in Rust
on [IronRDP](https://github.com/Devolutions/IronRDP). Its desktop UI uses egui,
winit, and glutin and is inspired by the Windows Remote Desktop client
(`mstsc`).

IronTSC supports saved `.rdp` connections, clipboard and audio redirection,
dynamic desktop resizing, RDP graphics codecs, TCP and UDP transports, and a
docked terminal.

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

Run `irontsc --help` for all connection, folder sharing, camera, audio capture
and configuration options. Nothing is logged by default; set `RUST_LOG` to opt
in, for example `RUST_LOG=debug irontsc`.

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

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.

The Microsoft protocol documents in `vendor/rdp-specs-md/` are not covered by
this license; they are Microsoft Open Specifications documentation reproduced
under Microsoft's terms. See [NOTICE](NOTICE).
