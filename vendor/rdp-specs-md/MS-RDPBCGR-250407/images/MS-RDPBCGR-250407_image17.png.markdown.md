# RDP Connection Establishment Sequence Diagram

This sequence diagram illustrates the step-by-step process for establishing a secure Remote Desktop Protocol (RDP) connection between a Client and a Server, including connection initiation, security negotiation, authentication, and session setup.

## Participants

- **Client**: Initiates the connection and performs authentication.
- **Server**: Accepts the connection request, negotiates security, and manages the session.

## Sequence of Events

The diagram is divided into four main phases, each marked by a curly brace annotation:

### 1. Connection Initiation

This phase establishes the basic network connection.

- **Client → Server**: `X.224 Connection Request PDU`
- **Server → Client**: `X.224 Connection Confirm PDU`

> *Note: The dashed lines with ellipses indicate that this phase may involve additional handshake messages not explicitly shown.*

### 2. TLS Security Protocol

This phase negotiates and establishes the Transport Layer Security (TLS) encryption layer.

- **Dashed lines with ellipses**: Represent multiple TLS handshake messages exchanged between Client and Server.
- The exact messages are not detailed in the diagram but are implied to be standard TLS negotiation messages (e.g., ClientHello, ServerHello, Certificate, KeyExchange, etc.).

### 3. RDSTLS Protocol (Encrypted and Wrapped by TLS Security Protocol)

This phase handles authentication using the RDP Security Token Layer (RDSTLS), which operates over the established TLS connection.

- **Server → Client**: `RDSTLS Capabilities PDU`
- **Client → Server**: `RDSTLS Authentication Request PDU`
- **Server → Client**: `RDSTLS Authentication Response PDU`

> *Note: The RDSTLS messages are explicitly labeled as being "encrypted and wrapped by TLS Security Protocol," indicating that the authentication exchange is protected by the TLS layer.*

### 4. Post-"Connection Initiation" RDP Traffic (Encrypted and Wrapped by TLS Security Protocol)

This phase involves the setup of the RDP session and subsequent RDP traffic.

- **Client → Server**: `MCS Connect Initial PDU with GCC Conference Create Request`
- **Server → Client**: `MCS Connect Response PDU with GCC Conference Create Response`
- **Client → Server**: `MCS Erect Domain Request PDU`
- **Dashed lines with ellipses**: Indicate additional MCS (Multi-Channel Service) messages may follow.

> *Note: All RDP traffic in this phase is explicitly labeled
