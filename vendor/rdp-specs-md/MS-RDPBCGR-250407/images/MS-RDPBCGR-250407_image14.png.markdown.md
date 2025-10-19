# RDP Connection Establishment Sequence Diagram

This diagram illustrates the sequence of messages exchanged between a Client and a Server during the establishment of a Remote Desktop Protocol (RDP) connection, including the security handshake and subsequent communication.

## Overview

The diagram is a sequence diagram with two main participants:
- **Client** (left side)
- **Server** (right side)

The interaction is divided into two main phases:
1. **CredSSP External Security Protocol Handshake** (top section)
2. **RDP Traffic Exchange** (bottom section)

## Phase 1: CredSSP External Security Protocol Handshake

This phase occurs before the RDP session begins and establishes secure authentication.

- The handshake is represented by two dashed lines between the Client and Server, indicating a secure negotiation process.
- The diagram labels this section as: **CredSSP External Security Protocol Handshake**

## Phase 2: RDP Traffic Exchange

This phase begins after the security handshake is complete and involves the exchange of RDP control messages.

### Message Flow

The following messages are exchanged between the Client and Server:

| Message Direction | Message Description | Notes |
|-------------------|---------------------|-------|
| Client → Server | X.224 Connection Request PDU | Initiates the connection request |
| Server → Client | X.224 Connection Confirm PDU | Confirms the connection request |
| Client → Server | MCS Connect Initial PDU with GCC Conference Create Request | Initiates the MCS (Microsoft Connection Service) conference |
| Server → Client | MCS Connect Response PDU with GCC Conference Create Response | Responds to the conference creation request |
| Client → Server | MCS Erect Domain Request PDU | Requests domain establishment (followed by ellipsis indicating additional messages) |

### Security Context

- All RDP traffic in this phase is **encrypted and wrapped by the CredSSP External Security Protocol**.
- This is indicated by a bracketed label on the right side of the diagram.

## Diagram Structure

- **Vertical Dashed Lines**: Represent the lifelines of the Client and Server.
- **Solid Arrows**: Represent message exchanges.
- **Dashed Boxes**: Enclose the RDP traffic exchange phase.
- **Labels**: Provide context for each message and phase.

The diagram clearly shows the progression from a security handshake to the establishment of encrypted RDP communication.
