# Clipboard Synchronization Diagram

This diagram illustrates the communication flow between two systems (A and B) for synchronizing clipboard data. It shows how local applications interact with their respective system clipboards and how these are connected via a virtual clipboard channel.

## System Components

Each system (A and B) contains the following components:

- **Local Application**: An application running locally within the system.
- **System Clipboard**: The clipboard service specific to the local system.
- **Virtual Channel End-Point**: A component that acts as an interface to the virtual clipboard channel.
- **Clipboard Virtual Channel**: A bidirectional communication channel connecting the two systems.

## Communication Flow

The diagram uses numbered arrows to represent the sequence of operations and data transfers.

### Within System A

- **1**: Local Application writes data to System Clipboard.
- **10**: System Clipboard sends data to Virtual Channel End-Point.
- **11**: Virtual Channel End-Point sends data to System Clipboard (possibly a confirmation or feedback).

### Within System B

- **7**: Local Application writes data to System Clipboard.
- **14**: System Clipboard sends data to Virtual Channel End-Point.
- **13**: Virtual Channel End-Point sends data to System Clipboard (possibly a confirmation or feedback).

### Cross-System Communication

- **2**: Virtual Channel End-Point in System A sends data to Clipboard Virtual Channel.
- **3**: Clipboard Virtual Channel sends data to Virtual Channel End-Point in System B.
- **4**: Virtual Channel End-Point in System B sends data to System Clipboard.
- **5**: System Clipboard in System B sends data to Virtual Channel End-Point.
- **6**: Virtual Channel End-Point in System B sends data to Clipboard Virtual Channel.
- **9**: Clipboard Virtual Channel sends data to Virtual Channel End-Point in System A.
- **12**: Virtual Channel End-Point in System A sends data to Clipboard Virtual Channel.
- **15**: Clipboard Virtual Channel sends data to Virtual Channel End-Point in System B.

## Summary Table

| Step | Source | Destination | Description |
|------|--------|-------------|-------------|
| 1 | Local Application (A) | System Clipboard (A) | Data written to local clipboard |
| 2 | Virtual Channel End-Point (A) | Clipboard Virtual Channel | Data sent to remote system |
| 3 | Clipboard Virtual Channel | Virtual Channel End-Point (B) | Data received by remote system |
| 4 | Virtual Channel
