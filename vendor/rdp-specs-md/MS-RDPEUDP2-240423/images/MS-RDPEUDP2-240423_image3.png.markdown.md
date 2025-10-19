# RDP-UDP2 Implementation Architecture

This diagram illustrates the layered structure of the RDP-UDP2 implementation, showing how higher layers of the RDP stack interact with the reliability mechanisms and the underlying UDP transport.

## Overview

The architecture is organized into a hierarchical stack, with the RDP-UDP2 implementation at the core, interfacing with higher RDP layers above and the UDP transport layer below.

## Key Components

### RDP-UDP2 Implementation (Main Module)
This is the central component that encapsulates the reliability mechanisms.

- **Reliability Controller**: Manages the overall reliability protocol.
- **Sender Window Buffer**: Buffers outgoing data for transmission.
- **Receiver Window Buffer**: Buffers incoming data for processing.
- **Loss Detection**: Monitors for missing or corrupted packets.

### Higher Layers of RDP Stack
- Located above the RDP-UDP2 implementation.
- Interact with it via a downward arrow, indicating data flow from higher layers into the implementation.

### UDP Transport
- Located at the bottom of the stack.
- Serves as the underlying transport layer for RDP-UDP2.
- Communicates with the RDP-UDP2 implementation via a downward arrow.

## Data Flow and Relationships

The following arrows indicate the direction of data or control flow:

- **From Higher Layers to RDP-UDP2**: A downward arrow shows that data from higher RDP layers is passed down to the RDP-UDP2 implementation.
- **From RDP-UDP2 to UDP Transport**: A downward arrow indicates that the RDP-UDP2 implementation sends data to the UDP transport for transmission.
- **Within RDP-UDP2 Implementation**: 
  - Arrows connect the **Sender Window Buffer** and **Receiver Window Buffer** to the **UDP transport**.
  - This suggests that these buffers interact with the transport layer for sending and receiving data.

## Summary Table

| Component                 | Description                                      | Direction of Interaction                     |
|--------------------------|--------------------------------------------------|----------------------------------------------|
| Higher Layers of RDP Stack | Application and protocol layers above RDP-UDP2   | Downward to RDP-UDP2 implementation          |
| RDP-UDP2 Implementation   | Core module containing reliability mechanisms    | Interfaces with higher layers and UDP transport |
| Reliability Controller    | Manages reliability protocol                     | Internal to RDP-UDP2 implementation          |
| Sender Window Buffer      | Buffers outgoing data                            | Interacts with UDP transport                 |
| Receiver Window Buffer    |
