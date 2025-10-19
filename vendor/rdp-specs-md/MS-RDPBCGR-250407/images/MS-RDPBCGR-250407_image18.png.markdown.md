# RDP Authentication and Connection Sequence Diagram

This sequence diagram illustrates the step-by-step interaction between a Client, Server, and Azure Active Directory (AAD) during an RDP (Remote Desktop Protocol) connection, including authentication and encryption setup.

## Participants

- **Client**: Initiates the connection and performs authentication.
- **Server**: Handles connection requests, coordinates authentication with AAD, and manages the RDP session.
- **AAD (Azure Active Directory)**: Provides authentication services and issues tokens.

## Sequence of Events

### 1. Connection Initiation

- **Client → Server**: X.224 Connection Request PDU
- **Server → Client**: X.224 Connection Confirm PDU
  - *Grouped under "Connection Initiation"*

### 2. RDP Access Token Acquisition

- **Client → AAD**: RDP Access Token request
- **AAD → Client**: RDP Access Token

### 3. AAD Nonce Exchange

- **Client → AAD**: AAD Nonce request
- **AAD → Client**: AAD Nonce

### 4. TLS Security Protocol Setup

- **Client ↔ Server**: Multiple messages exchanged (indicated by dotted lines)
  - *Grouped under "TLS Security Protocol"*
  - This phase establishes a secure, encrypted channel using TLS.

### 5. RDS AAD Authentication Protocol

- **Server → Client**: Server Nonce
- **Client → Server**: Authentication Request
- **Server → Client**: Authentication Result
  - *Grouped under "RDS AAD Auth Protocol (encrypted and wrapped by TLS Security Protocol)"*

### 6. MCS (Multi-Connection Service) Setup

- **Client → Server**: MCS Connect Initial PDU with GCC Conference Create Request
- **Server → Client**: MCS Connect Response PDU with GCC Conference Create Response
  - *Grouped under "The rest of the RDP protocol is encrypted and wrapped by TLS Security Protocol."*

### 7. Domain Setup

- **Client → Server**: MCS Erect Domain Request PDU

## Security Notes

- The **TLS Security Protocol** encrypts all subsequent communication.
- The **RDS AAD Auth Protocol** is encrypted and wrapped within the TLS protocol.
- The **rest of the RDP protocol** (after authentication) is also encrypted and wrapped by TLS.

This diagram outlines the secure handshake and authentication process necessary for establishing a remote desktop session with Azure Active Directory authentication.
