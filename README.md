# IronTSC - Modern Remote Desktop Client

A Remote Desktop Protocol (RDP) client built on **[IronRDP](https://github.com/Devolutions/IronRDP)**, with a shell drawn in **[egui](https://github.com/emilk/egui)** on winit and glutin. Inspired by the classic Windows `mstsc` (Microsoft Terminal Services Client).

The GUI drives winit and glutin directly rather than going through `eframe`, because RDP needs the *physical* position of every key: eframe only exposes egui's logical `Key`, which has no CapsLock, cannot tell the numpad from the number row, and folds AltGr into Alt. Keys are read from winit's `physical_key` and translated to Windows Set 1 scancodes.

## Building from Source

See **[BUILD.md](BUILD.md)** for the full dependency list per distribution. In short you need a Rust toolchain, a C toolchain with `cmake` and `clang`, the usual X11/Wayland/GL and ALSA development packages, OpenSSL, and **FFmpeg 8.0+** for the H.264 decoder.

```bash
git clone https://github.com/espentveit/irontsc.git
cd irontsc
cargo build --release
./target/release/irontsc
```

Release builds are 3-10x faster than debug builds; use them for anything but debugging.

If FFmpeg 8 is not available on your distribution, build without H.264 and video redirection:

```bash
cargo build --release --no-default-features --features gfx
```

## Usage

### Quick Start

Launch the application and fill in the connection dialog:

- **Server**: Hostname or IP address, optionally with `:port` (e.g. `server.example.com:3389`)
- **Username**: Your remote desktop username
- **Domain**: (Optional) Windows domain name
- **Password**: Your remote desktop password

Click **Connect**. The window opens at dialog size and grows to the session once connected.

### Options

**Show Options** grows the dialog into tabs, as `mstsc` does:

- **General** - the logon fields, and a **Connection settings** group: **Save**, **Save As...** and **Open...** for `.rdp` files.
- **Display** - remote desktop size (a Small-to-Large slider over 640x480, 800x600, 1024x768, 1920x1080 and full screen), colour depth, and DPI scaling.
- **Codecs** - H.264 hardware acceleration, and switches to disable AVC420 or AVC444.
- **Network** - force TCP by disabling UDP multitransport.
- **Debug** - outline each decoded region in the colour of the codec that produced it, the same overlay `RDP_DEBUG_CODEC_OUTLINES=1` turns on.

Every option is written to the `.rdp` file, so a saved connection carries its codec and transport choices with it.

### Session controls

The session controls float over the desktop as an island at the top of the window, in place of `mstsc`'s connection bar:

- **server name** - hover it for the connection details: user, resolution, DPI scale, transport, bandwidth, response time and frame rate. Drag it, or the `::` grip, to slide the island along the top edge.
- **Keys** - send Alt+Tab, Super and other system shortcuts to the remote desktop instead of the local one. Off by default. Uses the Wayland keyboard-shortcuts-inhibit protocol, or `XGrabKeyboard` on X11, and is always released on focus loss, disconnect and exit.
- **Pin** - keep the island visible in fullscreen. Unpinned, it hides in fullscreen and reappears once the pointer has rested at the top edge for a moment; in a window it stays put.
- **- / Full / X** - minimise, toggle fullscreen, disconnect.

### Command Line Options

```bash
irontsc [OPTIONS]

Options:
      --computer <COMPUTER>  Computer name or IP address, optionally with `:port`
  -u, --username <USERNAME>  User name to authenticate as
  -p, --password <PASSWORD>  Password to authenticate with
  -d, --domain <DOMAIN>      Domain to authenticate against
      --autologon            Connect immediately instead of showing the dialog
  -h, --help                 Print help
  -V, --version              Print version

Arguments:
  [FILE]  A .rdp file to load the settings from, the way `mstsc file.rdp` does
```

Anything short of a full set of credentials still opens the dialog, pre-filled.

Logging is controlled by `RUST_LOG`, e.g. `RUST_LOG=debug ./target/release/irontsc`.

## Configuration

Settings live in `~/.config/irontsc/default.rdp`, in the same `.rdp` format `mstsc` writes, so the file can be shared with it. The dialog is pre-filled from it, and its **Remember these settings** checkbox writes the server, user name and domain back.

The password is only saved when **Save password** is ticked, and it is stored in plaintext - leave it off unless you know what the file is for.

**Save As...** and **Open...** work on `.rdp` files anywhere, and `irontsc file.rdp` opens one straight from the command line. The file picker is the desktop's own, reached through `xdg-desktop-portal`; without a portal running, the buttons do nothing and the default file is still there.

## Debug Features

### Codec Visualization

Enable visual debugging of codec blocks with color-coded outlines:

```bash
RDP_DEBUG_CODEC_OUTLINES=1 ./target/release/irontsc
```

This will draw colored rectangles around decoded regions to identify which codec is being used:

- **Pink**: ClearCodec compressed tiles
- **Blue**: RFX Progressive codec tiles  
- **Green**: H.264/AVC420 frames
- **Yellow**: H.264/AVC444/AVC444v2 frames
- **Orange**: Uncompressed (raw BGRA) data

This feature is useful for:
- Understanding codec usage patterns
- Debugging graphics corruption issues
- Analyzing performance characteristics
- Verifying codec negotiation

## Technology Stack

- **[IronRDP](https://github.com/Devolutions/IronRDP)**: Pure Rust RDP protocol implementation by Devolutions
- **[egui](https://github.com/emilk/egui)** on **winit** and **glutin**: the window, the GL context and the event loop
- **FFmpeg**: H.264 (AVC420/AVC444) decoding for RDPEGFX

## MCP mode

IronTSC can hand a remote desktop to an AI agent over the [Model Context Protocol](https://modelcontextprotocol.io). The agent takes screenshots and sends clicks and keystrokes; there is one tool set, reachable two ways depending on who owns the session.

### In a session you are watching

Open the gear (`...`) in the session island and tick **MCP mode**. The agent joins the desktop already on screen, so you see everything it does as it happens, and you can take the mouse back at any moment. Nothing restarts.

The menu shows a URL to hand to a client:

```bash
claude mcp add --transport http irontsc 'http://127.0.0.1:7444/mcp?t=<token>'
```

The server binds to loopback only and requires the token in that URL, as a bearer header or the `t` parameter — anything running as your user could otherwise drive your desktop. A fresh token is generated each time MCP mode is switched on, so re-copy the URL after switching it off and on. The button reads **MCP** in blue while it is listening; switching it off releases anything the agent was holding down.

### Headless, with no window

For an agent that should open its own session, `irontsc mcp` speaks MCP over stdio — the form an MCP client starts for itself:

```bash
claude mcp add irontsc -- irontsc mcp --computer server.example.com -u user -p secret
```

It takes the same connection flags as the client, or a `.rdp` file. `--width` and `--height` set the desktop size. UDP multitransport is off unless you pass `--udp`: an agent works in clicks and screenshots, where a few milliseconds buy nothing and the extra transport is one more thing to go wrong. Logging goes to stderr here, because stdout is the protocol.

### Tools

| Tool | What it does |
| --- | --- |
| `screenshot` | The desktop as a PNG. Waits for the screen to stop changing first. `x`/`y`/`width`/`height` grab one region at full detail; `max_width` scales the result down to save tokens. |
| `pixel` | The exact colour at a point, or under the pointer if none is given. |
| `find_regions` | Flat rectangles on screen, largest first, as candidate controls. |
| `click`, `move_mouse`, `drag`, `scroll` | Mouse input, in desktop pixels. |
| `type_text` | Types as Unicode, so the server's keyboard layout need not match yours. |
| `key` | A chord: `enter`, `F5`, `ctrl+c`, `alt+tab`, `win+r`, `ctrl+alt+delete`. |
| `status` | Connection state, desktop size, and what the agent did recently. |
| `wait`, `disconnect` | Pause; close a session this server opened. |

Coordinates are always in the desktop's own pixels, never the screenshot's, and every reply that could be ambiguous says so.

`find_regions` deserves a caveat: it finds axis-aligned blocks of near-uniform colour, which Windows chrome is largely made of, so buttons and fields do turn up in it — but so does anything else that happens to be a flat rectangle. It is a hint to check against the image, not a widget tree. A model reading the screenshot remains the better way to find a control.

## Project Status

IronTSC is under active development. Still very experimental and not using all features from IronRDP.

## License

This project is licensed under the MIT License

## Acknowledgments

- **[IronRDP](https://github.com/Devolutions/IronRDP)** by Devolutions for the excellent Rust RDP library
- **Microsoft** for the `mstsc` client that inspired this project's design
- **[egui](https://github.com/emilk/egui)** and the winit and glutin projects for a GUI stack that ports without a desktop toolkit behind it

## Related Projects

- [IronRDP](https://github.com/Devolutions/IronRDP) - The RDP library powering this client
- [Remmina](https://remmina.org/) - Feature-rich remote desktop client with multi-protocol support
- [FreeRDP](https://www.freerdp.com/) - Open-source RDP client implementation

## Support

For issues, questions, or feature requests, please open an issue on the [GitHub repository](https://github.com/espentveit/irontsc/issues).
