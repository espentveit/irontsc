# Sequence Diagram: Server-Client Interaction for Desktop Composition

This sequence diagram illustrates the interaction between a Server and a Client regarding desktop composition state changes, specifically toggling between composition on and off, which involves desktop switch cycles.

## Participants

- **Server**: Initiates the sequence by sending messages to the Client.
- **Client**: Receives and processes messages from the Server.

## Message Flow

The diagram shows a sequence of messages sent from the Server to the Client, grouped into two main phases:

### Phase 1: Composition On

- **Message**: `COMPDESKTOGGLE - COMPOSITION_ON`
  - This message triggers the Client to enter composition mode.

### Phase 2: Desktop Switch Cycle (First)

- **Message**: `COMPDESKTOGGLE - DMW_DESK_LEAVE`
  - The Client responds by leaving the current desktop.
- **Message**: `COMPDESKTOGGLE - DMW_DESK_ENTER`
  - The Client then enters a new desktop.

> This sequence is labeled as a **Desktop Switch Cycle**.

### Phase 3: Desktop Switch Cycle (Second)

- **Message**: `COMPDESKTOGGLE - DMW_DESK_LEAVE`
  - The Client leaves the current desktop again.
- **Message**: `COMPDESKTOGGLE - DMW_DESK_ENTER`
  - The Client enters a new desktop.

> This sequence is also labeled as a **Desktop Switch Cycle**.

### Phase 4: Composition Off

- **Message**: `COMPDESKTOGGLE - COMPOSITION_OFF`
  - This message triggers the Client to exit composition mode.

## Groupings

- **Desktop Switch Cycle**: A bracketed group encompassing the two `DMW_DESK_LEAVE` and `DMW_DESK_ENTER` message pairs.
- **Composition Cycle**: A larger bracketed group encompassing both Desktop Switch Cycles and the `COMPOSITION_ON` and `COMPOSITION_OFF` messages.

## Summary Table

| Message Type             | Description                     | Triggered By          |
|--------------------------|---------------------------------|------------------------|
| `COMPDESKTOGGLE - COMPOSITION_ON` | Initiates composition mode     | Server                |
| `COMPDESKTOGGLE - DMW_DESK_LEAVE` | Client leaves current desktop  | Server (during cycle) |
| `COMPDESKTOGGLE - DMW_DESK_ENTER` | Client enters new desktop      | Server (during cycle) |
| `COMPDESKTOGGLE - COMPOSITION_OFF` |
