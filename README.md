# IronTSC - Modern Remote Desktop Client

A modern Remote Desktop Protocol (RDP) client built with **[IronRDP](https://github.com/Devolutions/IronRDP)** and **GTK4**. Inspired by the classic Windows `mstsc` (Microsoft Terminal Services Client).

## Prerequisites

### Fedora / RHEL / CentOS
```bash
sudo dnf install gtk4-devel libadwaita-devel cairo-devel alsa-lib-devel rust cargo pkg-config
```

### Ubuntu / Debian
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libcairo2-dev libasound2-dev rustc cargo pkg-config build-essential
```

### Arch Linux / Manjaro
```bash
sudo pacman -S gtk4 libadwaita cairo alsa-lib rust cargo pkgconf base-devel
```

### Other Dependencies
- **Rust toolchain**: 1.70 or later (install from [rustup.rs](https://rustup.rs/))
- **Git**: For cloning the repository

## Building from Source

1. **Clone the repository**:
   ```bash
   git clone https://github.com/espentveit/irontsc.git
   cd irontsc
   ```

2. **Build in release mode** (recommended for best performance):
   ```bash
   cargo build --release
   ```

3. **Run the application**:
   ```bash
   ./target/release/irontsc
   ```

   Or build and run in one step:
   ```bash
   cargo run --release
   ```

## Usage

### Quick Start
Launch the application and enter your connection details:
- **Server**: Hostname or IP address (e.g., `server.example.com`)
- **Username**: Your remote desktop username
- **Domain**: (Optional) Windows domain name
- **Password**: Your remote desktop password

Click **Connect** to establish the connection.

### Command Line Options
```bash
irontsc [OPTIONS]

Options:
  -h, --help     Print help
  -V, --version  Print version
```
## Configuration

Connection settings are automatically saved to:
- Linux: `~/.config/irontsc/config.json`

The configuration file stores:
- Last used server address
- Username and domain
- Connection preferences

## Performance Optimization

For best performance:
1. **Always use release builds**: `cargo build --release` (3-10x faster than debug builds)

## Technology Stack

- **[IronRDP](https://github.com/Devolutions/IronRDP)**: Pure Rust RDP protocol implementation by Devolutions
- **GTK4**: Modern cross-platform GUI toolkit

## Project Status

IronTSC is under active development. Still very experimental and not using all features from IronRDP.

## License

This project is licensed under the MIT License

## Acknowledgments

- **[IronRDP](https://github.com/Devolutions/IronRDP)** by Devolutions for the excellent Rust RDP library
- **Microsoft** for the `mstsc` client that inspired this project's design
- The GTK and GNOME communities for the outstanding toolkit and design guidelines

## Related Projects

- [IronRDP](https://github.com/Devolutions/IronRDP) - The RDP library powering this client
- [Remmina](https://remmina.org/) - Feature-rich remote desktop client with multi-protocol support
- [FreeRDP](https://www.freerdp.com/) - Open-source RDP client implementation

## Support

For issues, questions, or feature requests, please open an issue on the [GitHub repository](https://github.com/espentveit/irontsc/issues).
