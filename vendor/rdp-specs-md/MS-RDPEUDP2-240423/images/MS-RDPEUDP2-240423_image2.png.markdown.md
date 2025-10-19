# RDP UDP Connection Flowchart

This flowchart illustrates the sequence of phases in an RDP (Remote Desktop Protocol) UDP connection, detailing the initialization and data transfer stages based on protocol version negotiation.

## Flow Overview

The process begins with the establishment of a UDP connection and proceeds through initialization, protocol version negotiation, and then into one of two data transfer phases.

## Key Phases and Decision Points

### 1. Initial Connection
- **Start**: `UDP connection established`

### 2. Connection Initialization
- **Phase**: `Connection initialization phase (MS-RDPEUDP)`
  - This phase follows the initial UDP connection setup.

### 3. Protocol Version Negotiation
- **Decision Point**: `Negotiated RDPUDP_PROTOCOL_VERSION > RDPUDP_PROTOCOL_VERSION_2`
  - This decision determines which data transfer phase will be used.

#### Branches Based on Negotiation Result

| Condition | Outcome | Next Phase |
|-----------|---------|------------|
| **Yes** (Negotiated version > RDPUDP_PROTOCOL_VERSION_2) | Protocol version is greater than version 2 | `RDP UDP Data Transfer Phase v2 (MS-RDPEUDP2)` |
| **No** (Negotiated version ≤ RDPUDP_PROTOCOL_VERSION_2) | Protocol version is not greater than version 2 | `RDP UDP Data Transfer Phase (MS-RDPEUDP)` |

## Summary of Data Transfer Phases

- **RDP UDP Data Transfer Phase v2 (MS-RDPEUDP2)**: Used when the negotiated protocol version exceeds version 2.
- **RDP UDP Data Transfer Phase (MS-RDPEUDP)**: Used when the negotiated protocol version is version 2 or lower.

This flowchart shows a clear, conditional progression from connection establishment to data transfer, with the protocol version acting as the key decision factor.
