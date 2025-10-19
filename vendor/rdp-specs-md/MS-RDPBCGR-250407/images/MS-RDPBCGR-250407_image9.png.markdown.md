# Sequence Diagram: Client-Server Security Exchange

This sequence diagram illustrates the initial phase of a secure communication protocol between a Client and a Server, focusing on the exchange of security data.

## Participants

- **Client**: Initiates the security exchange.
- **Server**: Responds with security data and receives the Client's random value.

## Message Flow

The diagram depicts two sequential messages exchanged between the Client and Server:

### Message 1: Server to Client
- **Direction**: From Server → Client
- **Content**: `Server Security Data: Server Random and Certificate`
- **Description**: The Server sends its random value and its digital certificate to the Client. This establishes the Server's identity and provides the public key needed for encryption.

### Message 2: Client to Server
- **Direction**: From Client → Server
- **Content**: `Security Exchange PDU: Client Random (Encrypted with Server's Public Key)`
- **Description**: The Client sends its own random value, encrypted using the Server's public key (obtained from the certificate in the first message). This ensures that only the Server can decrypt and read the Client's random value.

## Diagram Structure

The diagram uses standard sequence diagram notation:
- Rectangular boxes represent the participants (`Client`, `Server`).
- Vertical dashed lines represent the lifelines of each participant.
- Arrows represent messages exchanged between participants.
- Labels on arrows describe the content and direction of each message.

This exchange is a foundational step in establishing a secure channel, typically used in protocols like TLS/SSL to set up shared secrets for subsequent symmetric encryption.
