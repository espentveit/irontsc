# Building IronTSC

## Requirements

- Rust toolchain (1.70+)
- **FFmpeg 8.0+** (required for `h264` and `video-redirection` features)

## Install dependencies

### Ubuntu / Debian

```bash
sudo apt install -y \
    cargo rustc pkg-config build-essential \
    cmake clang libclang-dev \
    libxkbcommon-dev libwayland-dev libgl1-mesa-dev libx11-dev \
    libasound2-dev libssl-dev \
    libavcodec-dev libavformat-dev libavutil-dev \
    libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev \
    libfdk-aac-dev libopus-dev
```

Requires Ubuntu 25.10+ (or Debian equivalent) for FFmpeg 8. On older
releases, use PPA builds of FFmpeg 8 or build without the `h264` /
`video-redirection` features (`--no-default-features --features gfx`).

### Fedora / RHEL / CentOS

```bash
sudo dnf install -y \
    rust cargo pkg-config \
    cmake clang-devel \
    libxkbcommon-devel wayland-devel mesa-libGL-devel libX11-devel \
    alsa-lib-devel openssl-devel \
    ffmpeg-devel fdk-aac-devel opus-devel
```

### Arch / Manjaro

```bash
sudo pacman -S --needed \
    rust cargo pkgconf base-devel \
    cmake clang \
    libxkbcommon wayland mesa libx11 alsa-lib openssl \
    ffmpeg fdk-aac opus
```

## Build

Release build (recommended):

```bash
cargo build --release
```

The resulting binary is at `target/release/irontsc`.

To build without H.264 / video redirection (no FFmpeg required):

```bash
cargo build --release --no-default-features --features gfx
```

MCP mode (`irontsc mcp`, the gear toggle) is opt-in and left out of the default build:

```bash
cargo build --release --features mcp
```

## Run

```bash
./target/release/irontsc
```

Or in one step:

```bash
cargo run --release
```
