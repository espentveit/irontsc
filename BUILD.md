# Building IronTSC

## Install dependencies

### Ubuntu / Debian

```bash
sudo apt install -y \
    cargo \
    rustc \
    pkg-config \
    build-essential \
    libgtk-4-dev \
    libadwaita-1-dev \
    libcairo2-dev \
    libasound2-dev \
    libssl-dev
```

### Fedora / RHEL / CentOS

```bash
sudo dnf install -y \
    rust cargo pkg-config \
    gtk4-devel libadwaita-devel cairo-devel \
    alsa-lib-devel openssl-devel
```

### Arch / Manjaro

```bash
sudo pacman -S --needed \
    rust cargo pkgconf base-devel \
    gtk4 libadwaita cairo alsa-lib openssl
```

## Build

Release build (recommended):

```bash
cargo build --release
```

The resulting binary is at `target/release/irontsc`.

## Run

```bash
./target/release/irontsc
```

Or in one step:

```bash
cargo run --release
```
