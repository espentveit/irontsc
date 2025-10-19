# Sequence Diagram: Client-Server Security Negotiation

This diagram illustrates the exchange of security data between a Client and a Server during an initial handshake or negotiation phase, specifically focusing on encryption method selection.

## Diagram Structure

The diagram is a sequence diagram with two participants:

- **Client** (on the left)
- **Server** (on the right)

Each participant has a vertical dashed line representing their timeline. Horizontal arrows indicate messages exchanged between them.

## Message Flow

The sequence of messages is as follows:

1.  **Client → Server**: Client Security Data: Supported Encryption Methods
    - The Client sends a message to the Server listing the encryption methods it supports.
    - This is an outbound message from the Client to the Server.

2.  **Server → Client**: Server Security Data: Selected Encryption Method and Encryption Level
    - The Server responds by sending a message back to the Client.
    - This message contains the encryption method and level that the Server has selected from the Client's list.
    - This is an outbound message from the Server to the Client.

## Summary Table

| Message Direction | Sender     | Receiver | Message Content                                   |
|-------------------|------------|----------|---------------------------------------------------|
| Client → Server   | Client     | Server   | Client Security Data: Supported Encryption Methods |
| Server → Client   | Server     | Client   | Server Security Data: Selected Encryption Method and Encryption Level |

This exchange is a fundamental part of establishing a secure communication channel, ensuring both parties agree on the encryption parameters before any encrypted data is transmitted.
