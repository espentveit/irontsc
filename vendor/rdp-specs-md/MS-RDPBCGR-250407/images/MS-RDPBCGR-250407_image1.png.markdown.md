# RDP Connection Sequence Diagram

This sequence diagram illustrates the communication protocol between a Client and a Server during a Remote Desktop Protocol (RDP) connection. The process is divided into several phases, each marked with a bracketed label on the right side.

## Participants

- **Client**: Initiates the connection and exchanges information with the server.
- **Server**: Responds to the client's requests and manages the connection setup.

## Connection Phases

### 1. Connection Initiation
- **Client → Server**: X.224 Connection Request PDU
- **Server → Client**: X.224 Connection Confirm PDU

### 2. Basic Settings Exchange
- **Client → Server**: MCS Connect Initial PDU with GCC Conference Create Request
- **Server → Client**: MCS Connect Response PDU with GCC Conference Create Response

### 3. Channel Connection
- **Client → Server**: MCS Erect Domain Request PDU
- **Client → Server**: MCS Attach User Request PDU
- **Server → Client**: MCS Attach User Confirm PDU
- **Client → Server**: MCS Channel Join Request PDU(s)
- **Server → Client**: MCS Channel Join Confirm PDU(s)

### 4. RDP Security Commencement
- **Client → Server**: Security Exchange PDU

### 5. Secure Settings Exchange
- **Client → Server**: Client Info PDU

### 6. Optional Connect-Time Auto-Detection
- **Client → Server**: Auto-Detect Request PDU(s)
- **Server → Client**: Auto-Detect Response PDU(s)

### 7. Licensing
- **Server → Client**: License Error PDU – Valid Client

### 8. Optional Multitransport Bootstrapping
- **Client → Server**: Initiate Multitransport Request PDU
- **Server → Client**: Initiate Multitransport Response PDU

### 9. Capabilities Exchange
- **Server → Client**: Demand Active PDU
- **Server → Client**: Monitor Layout PDU
- **Client → Server**: Confirm Active PDU

### 10. Connection Finalization
- **Client → Server**: Synchronize PDU
- **Client → Server**: Control PDU - Cooperate
- **Client → Server**: Control PDU – Request Control
- **Client → Server**: Persistent Key List PDU(s)
- **Client → Server**: Font List PDU
- **Server → Client**: Synchronize P
