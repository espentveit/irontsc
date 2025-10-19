# Sequence Diagram: Server-Client Interaction

This sequence diagram illustrates the communication flow between a Server and a Client, detailing the steps involved in initialization, authentication, and device management.

## Participants

- **Server**: Initiates communication and responds to client messages.
- **Client**: Sends messages to the server and receives responses.

## Communication Flow

The interaction is broken down into two main phases, each grouped with a descriptive label.

### Phase 1: Versioning and Initialization

This phase establishes compatibility and sets up the connection.

1. **Server Version / Capabilities Message**
   - Sent from Server to Client.
   - Purpose: Inform the client about the server's version and capabilities.

2. **Client Version / Capabilities Response**
   - Sent from Client to Server.
   - Purpose: Respond with the client's version and capabilities.

### Phase 2: Device Addition/Removal

This phase handles the management of devices connected to the client.

1. **Authenticated Client Message**
   - Sent from Server to Client.
   - Purpose: Indicates that the client has been successfully authenticated.

2. **Client Device Additions Message**
   - Sent from Client to Server.
   - Purpose: Notify the server of newly added devices.

3. **Client Device Removal Message**
   - Sent from Client to Server.
   - Purpose: Notify the server of devices that have been removed.

## Message Summary Table

| Message Type                      | Direction       | Sender   | Receiver | Purpose                                  |
|----------------------------------|-----------------|----------|----------|------------------------------------------|
| Server Version / Capabilities Message | Server → Client | Server   | Client   | Inform client of server version/capabilities |
| Client Version / Capabilities Response | Client → Server | Client   | Server   | Respond with client version/capabilities   |
| Authenticated Client Message     | Server → Client | Server   | Client   | Confirm client authentication             |
| Client Device Additions Message  | Client → Server | Client   | Server   | Notify of device additions               |
| Client Device Removal Message    | Client → Server | Client   | Server   | Notify of device removals                |

## Groupings

The diagram uses curly braces `{}` to group related messages under descriptive labels:

- **Versioning and initialization**: Covers the first two messages.
- **Device Addition/Removal**: Covers the last three messages.

This structure helps to visualize the logical phases of the interaction.
