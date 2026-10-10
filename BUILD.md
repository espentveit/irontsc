# Building IronTSC

## Requirements

- A stable Rust toolchain from [rustup](https://rustup.rs/)
- The platform's native development packages, below
- **FFmpeg 8** development libraries, only for the `h264` and
  `video-redirection` features (see [Features](#features))

## Install dependencies

### Ubuntu / Debian

```bash
sudo apt install -y \
    pkg-config build-essential \
    cmake clang libclang-dev \
    libxkbcommon-dev libwayland-dev libgl1-mesa-dev libx11-dev \
    libasound2-dev libssl-dev \
    libavcodec-dev libavformat-dev libavutil-dev \
    libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev \
    libfdk-aac-dev libopus-dev
```

FFmpeg 8 needs Ubuntu 25.10+ (or the Debian equivalent). On older releases,
use a PPA build of FFmpeg 8 or build without H.264 (see below).

### Fedora / RHEL / CentOS

```bash
sudo dnf install -y \
    pkg-config cmake clang-devel \
    libxkbcommon-devel wayland-devel mesa-libGL-devel libX11-devel \
    alsa-lib-devel openssl-devel \
    ffmpeg-devel fdk-aac-devel opus-devel
```

### Arch / Manjaro

```bash
sudo pacman -S --needed \
    pkgconf base-devel cmake clang \
    libxkbcommon wayland mesa libx11 alsa-lib openssl \
    ffmpeg fdk-aac opus
```

### Windows

Visual Studio Build Tools with the C++ workload, LLVM (for `libclang`), CMake,
and OpenSSL (for example from vcpkg, with `OPENSSL_DIR` pointing at it). The
MSI is built with WiX Toolset v3.

## Build

The full client, with H.264:

```bash
cargo build --locked --release
```

Without FFmpeg -- the feature set the published downloads use:

```bash
cargo build --locked --release --no-default-features --features gfx
```

The executable is `target/release/irontsc` on Linux and
`target\release\irontsc.exe` on Windows. Release builds on Windows are a
windowed app; run from a terminal, they still print to it.

## Features

| Feature | Default | What it adds |
| --- | --- | --- |
| `gfx` | yes | Nothing on its own: names the portable set, `--no-default-features --features gfx` |
| `h264` | yes | H.264 (AVC420/AVC444) decoding through FFmpeg 8 |
| `video-redirection` | yes | Video redirection, also through FFmpeg 8 |
| `mcp` | no | MCP mode: `irontsc mcp` and the session toolbar's gear, for agent-driven sessions |

```bash
cargo build --release --features mcp
```

## Usage

```sh
irontsc                                   # the connection dialog
irontsc connection.rdp                    # a saved connection
irontsc --computer server.example.com --username user --password secret --autologon
```

`irontsc --help` lists every option: folder sharing (`--share name=/path`),
camera, microphone, NLA and more. Preferences that apply to every connection
are read and written with `irontsc config`.

Nothing is logged by default. Set `RUST_LOG` to opt in, for example
`RUST_LOG=debug irontsc`.

## Packages and CI

`.github/workflows/release.yml` builds on every push to `main`, pull request
and manual run, and keeps the packages as workflow artifacts:

| Platform | Packages |
| --- | --- |
| Linux x86-64 | `.tar.gz` archive |
| Windows x64 | Portable ZIP and MSI installer |

Tags such as `v0.1.0` are also published as a GitHub Release with a
`SHA256SUMS` file. The packages use the `gfx` feature set, without H.264 or
video redirection.

`packaging/windows/package.ps1` builds the ZIP and MSI from a release build.

## Icon

The icon is drawn by a script, which writes every size the builds use
(`irontsc.ico`, the PNGs and the raw RGBA window icon):

```bash
python3 packaging/assets/make_icon.py
```

`build.rs` embeds the `.ico` in `irontsc.exe` on Windows.

## Development

```bash
cargo test --workspace
```

- `crates/` holds the vendored, patched IronRDP crates, the UDP transport
  (`irontsc-udp`) and the ported codecs.
- `vendor/rdp-specs-md/` holds the Microsoft protocol documents used as
  implementation references.
