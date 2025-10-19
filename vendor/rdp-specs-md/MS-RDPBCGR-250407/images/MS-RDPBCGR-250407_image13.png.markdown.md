# RDP Connection Sequence Diagram

This diagram illustrates the sequence of messages exchanged between a Client and a Server during an RDP (Remote Desktop Protocol) connection establishment, including security handshake and user authorization phases.

## Key Components

- **Client**: Initiates the connection and receives responses from the Server.
- **Server**: Responds to the Client's requests and manages the connection lifecycle.

## Sequence Phases

### 1. Connection Initiation

- **Client → Server**: `X.224 Connection Request PDU`
- **Server → Client**: `X.224 Connection Confirm PDU`

> *This phase establishes the initial communication channel.*

### 2. External Security Protocol Handshake

- A series of dashed arrows indicate multiple messages exchanged between Client and Server.
- This phase is labeled as **External Security Protocol Handshake**.

> *This phase typically involves negotiation and setup of security parameters, such as encryption keys.*

### 3. Optional User Authorization Result

- **Server → Client**: `Early User Authorization Result PDU`
- This phase is labeled as **Optional User Authorization Result**.

> *This message may be sent before the full RDP session is established, providing early authorization feedback.*

### 4. RDP Session Setup (Post-Connection Initiation)

- **Client → Server**: `MCS Connect Initial PDU with GCC Conference Create Request`
- **Server → Client**: `MCS Connect Response PDU with GCC Conference Create Response`
- **Client → Server**: `MCS Erect Domain Request PDU`

> *These messages establish the MCS (Microsoft Connection Service) and GCC (Gateway Conference Control) components of the RDP session.*

### 5. Post-“Connection Initiation” RDP Traffic Encryption

- A note indicates that **Post-"Connection Initiation" RDP Traffic** is:
  - Encrypted
  - Wrapped by the **External Security Protocol**

> *This ensures that all subsequent RDP traffic is protected by the security protocol established during the handshake phase.*

## Message Flow Summary

| Phase                         | Client Action                                      | Server Action                                     | Notes                                 |
|------------------------------|----------------------------------------------------|---------------------------------------------------|---------------------------------------|
| Connection Initiation        | Sends `X.224 Connection Request PDU`               | Sends `X.224 Connection Confirm PDU`              | Establishes initial channel           |
| External Security Handshake  | Exchanges multiple messages (dashed arrows)        | Exchanges multiple messages (dashed arrows)
