# UDP Connection and Data Transfer Sequence Diagram

This diagram illustrates a communication sequence between a Terminal client and a Terminal server using a modified TCP-like handshake over UDP.

## Participants

- **Terminal client**: Initiates the connection and receives data.
- **Terminal server**: Accepts the connection and sends data.

## Communication Sequence

The sequence is divided into two main phases:

### 1. UDP Connection Initialization

This phase establishes the connection between the client and server.

- **Step 1**: Terminal client → Terminal server: `SYN`
  - The client sends a SYN (synchronize) packet to initiate the connection.

- **Step 2**: Terminal server → Terminal client: `SYN + ACK`
  - The server responds with a SYN-ACK packet, acknowledging the client's SYN and synchronizing its own sequence number.

- **Step 3**: Terminal client → Terminal server: `ACK + Coded Packet`
  - The client sends an ACK (acknowledge) packet to confirm receipt of the server's SYN-ACK, along with the first data packet, which is coded.

### 2. UDP Data Transfer

This phase involves the actual transfer of data packets.

- **Step 4**: Terminal client → Terminal server: `ACK + Coded Packet`
  - The client sends another ACK along with a coded data packet. This indicates the client is ready to receive more data and is acknowledging the previous packet.

- **Step 5**: Terminal server → Terminal client: `ACK`
  - The server sends an ACK packet to acknowledge the receipt of the client's ACK + Coded Packet.

## Grouped Phases

The diagram uses curly braces to group the steps into logical phases:

- **UDP connection initialization**: Includes Steps 1, 2, and 3.
- **UDP data transfer**: Includes Steps 4 and 5.

## Notes

- The protocol uses `ACK` (acknowledge) to confirm receipt of packets.
- The term "Coded Packet" suggests that data packets are encoded for transmission, possibly for error correction or security.
- This protocol appears to be a custom or modified protocol that uses a TCP-like handshake over UDP, which is not standard for UDP (which is connectionless). This might be used in scenarios requiring reliable delivery over UDP.
