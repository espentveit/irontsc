# TCP Connection State Diagram: Terminal Server vs. Terminal Client

This diagram illustrates the state transitions for TCP connection establishment and termination for two entities: a **Terminal server** and a **Terminal client**. Each follows a distinct path based on its role in the TCP handshake.

---

## Terminal Server State Diagram

The server waits for incoming connections and initiates the listening state.

### States

- **Closed** (Initial state)
- **Listen** (Waiting for incoming connection)
- **SYN Received** (Received a SYN from a client)
- **Established** (Connection fully established)

### Transitions

| Transition Trigger       | From State   | To State       |
|--------------------------|--------------|----------------|
| `Listen`                 | Closed       | Listen         |
| `SYN/SYN+ACK`            | Listen       | SYN Received   |
| `ACK`                    | SYN Received | Established     |
| `Close`                  | Established  | Closed         |
| `Close network connection` | Closed      | Termination (Final State) |

---

## Terminal Client State Diagram

The client initiates the connection by sending a SYN.

### States

- **Closed** (Initial state)
- **SYN Sent** (Sent SYN to server)
- **Established** (Connection fully established)

### Transitions

| Transition Trigger       | From State   | To State       |
|--------------------------|--------------|----------------|
| `Connect/SYN`            | Closed       | SYN Sent       |
| `SYN+ACK/ACK(+DATA)`     | SYN Sent     | Established     |
| `Close`                  | Established  | Closed         |
| `Close network connection` | Closed      | Termination (Final State) |

---

## Key Observations

- **Symmetry**: Both diagrams follow the standard TCP 3-way handshake (SYN, SYN+ACK, ACK) but from different perspectives.
- **Termination**: Both entities can terminate the connection by sending a `Close` command, returning to the `Closed` state. A `Close network connection` action leads to the final termination state (double circle).
- **Role Difference**:
  - The **server** starts in `Listen` after `Closed`, waiting for incoming connections.
  - The **client** starts in `Closed`, then sends a `Connect/SYN` to initiate the connection.
- **Established State**: Both reach `Established` after completing the handshake and can then send or receive data. From here
