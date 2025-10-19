# Remote Desktop Client and Terminal Server Interaction Diagram

This sequence diagram illustrates the communication flow between a Remote Desktop Client and a Terminal Server during a license and security handshake process.

## Participants

- **Remote Desktop Client**: Initiates the connection and responds to server requests.
- **Terminal Server**: Responds to the client's requests and provides license information.

## Communication Sequence

The diagram shows a two-step interaction:

### Step 1: Server License Request
- **Direction**: From Terminal Server → Remote Desktop Client
- **Message Content**: 
  - `Server License Request:`
  - `Server Random and Certificate`
- **Purpose**: The server sends its license request, including a random value and a certificate, to the client.

### Step 2: New License Request / License Info
- **Direction**: From Remote Desktop Client → Terminal Server
- **Message Content**:
  - `New License Request/License Info:`
  - `Client Random and Encrypted PreMaster Secret`
- **Purpose**: The client responds with its own random value and an encrypted pre-master secret to establish a secure session.

## Diagram Structure

The diagram uses vertical dashed lines to represent the lifelines of each participant, with horizontal arrows indicating message flow. The messages are labeled with their content and purpose. The arrows show the direction of communication, with the first message going from server to client and the second from client to server.
