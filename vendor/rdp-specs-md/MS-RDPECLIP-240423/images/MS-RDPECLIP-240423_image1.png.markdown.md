# Sequence Diagram: Client-Server Clipboard Initialization

This sequence diagram illustrates the initial handshake and capability exchange between a Client and a Server, specifically for clipboard operations. The interactions are ordered chronologically from top to bottom.

## Participants

- **Client**: Initiates requests and receives responses from the Server.
- **Server**: Responds to Client requests and sends initial capabilities.

## Message Flow

The diagram shows a series of Protocol Data Units (PDUs) exchanged between the Client and Server. The arrows indicate direction of communication.

| Message Type | Direction | Description |
|--------------|-----------|-------------|
| Server Clipboard Capabilities PDU | Server → Client | The Server sends its clipboard capabilities to the Client. |
| Monitor Ready PDU | Server → Client | The Server notifies the Client that the monitor is ready. |
| Client Clipboard Capabilities PDU | Client → Server | The Client sends its own clipboard capabilities to the Server. |
| Temporary Directory PDU | Client → Server | The Client provides a temporary directory path to the Server. |
| Format List PDU | Client → Server | The Client requests a list of supported clipboard formats from the Server. |
| Format List Response PDU | Server → Client | The Server responds with the list of supported clipboard formats. |

## Structure

- The diagram uses vertical dashed lines to represent the lifelines of each participant (Client and Server).
- Horizontal arrows represent messages exchanged between the participants.
- All messages are labeled with their type (e.g., "Server Clipboard Capabilities PDU").
- The sequence flows from top to bottom, indicating the chronological order of events.

## Key Observations

- The Server initiates the communication by sending two messages to the Client before the Client responds.
- The Client then sends three messages to the Server, followed by a response from the Server.
- The exchange is symmetric: the Server provides initial information, and the Client provides its own capabilities and requests specific data (format list).
