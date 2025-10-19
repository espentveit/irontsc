# USB Device Lifecycle Sequence Diagram

This sequence diagram illustrates the interaction between a USB hardware device, client and server protocols, and the USB driver stack during device plug-in and unplugging events.

## Participants

- **USB hardware device**: The physical USB device that connects and disconnects.
- **Client protocol**: The client-side protocol layer that manages device communication.
- **Server protocol**: The server-side protocol layer that handles device communication and interacts with the driver stack.
- **USB driver stack**: The low-level driver component that manages USB device communication with the operating system.

## Sequence of Events

### Device Plugged In

1. **Device plugged in** → Triggered by the physical connection of the USB device.
2. **Add virtual channel message** → Sent from Client protocol to Server protocol.
3. **Channel Create message** → Sent from Server protocol to Client protocol.
4. **Channel Create message** → Sent from Client protocol to Server protocol.
5. **Add Device message** → Sent from Client protocol to Server protocol.
6. **Create driver stack** → Sent from Server protocol to USB driver stack.
7. **I/O request** → Sent from Client protocol to Server protocol.
8. **I/O response** → Sent from Server protocol to Client protocol.

### Device Unplugged

1. **Device unplugged** → Triggered by the physical disconnection of the USB device.
2. **Channel close** → Sent from Client protocol to Server protocol.
3. **Destroy driver stack** → Sent from Server protocol to USB driver stack.

## Communication Flow

| Event | Sender | Receiver | Description |
|-------|--------|----------|-------------|
| Device plugged in | USB hardware device | Client protocol | Initiates device connection |
| Add virtual channel message | Client protocol | Server protocol | Requests virtual channel setup |
| Channel Create message | Server protocol | Client protocol | Confirms channel creation |
| Channel Create message | Client protocol | Server protocol | Client acknowledges channel creation |
| Add Device message | Client protocol | Server protocol | Registers the device |
| Create driver stack | Server protocol | USB driver stack | Initializes driver for device communication |
| I/O request | Client protocol | Server protocol | Client requests I/O operation |
| I/O response | Server protocol | Client protocol | Server returns I/O result |
| Device unplugged | USB hardware device | Client protocol | Initiates device disconnection |
| Channel close | Client protocol | Server protocol | Closes communication channel |
| Destroy driver stack | Server protocol |
