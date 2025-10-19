# Sequence Diagram: Server-Client Communication

This sequence diagram illustrates the communication protocol between a Server and a Client, detailing the exchange of messages for establishing capabilities and performing device I/O operations.

## Participants

- **Server**: Initiates and responds to requests from the Client.
- **Client**: Sends requests to the Server and receives replies.

## Communication Phases

### Phase 1: Version/Capabilities Exchange

This initial phase establishes the communication parameters between the Server and Client.

- **Server → Client**: `Capabilities Request`
- **Client → Server**: `Capabilities Reply`

> *Note: This exchange is labeled "Version/Capabilities" in the diagram.*

### Phase 2: Device I/O Operations

This phase encompasses a series of requests and replies for performing input/output operations on a device.

- **Server → Client**: `CreateFile Request`
- **Client → Server**: `CreateFile Reply`

- **Server → Client**: `Read Request`
- **Client → Server**: `Read Reply`

- **Server → Client**: `Write Request`
- **Client → Server**: `Write Reply`

> *Note: This group of operations is labeled "Device IO" in the diagram.*

- **Server → Client**: `IoControl Request`
- **Client → Server**: `IoControl Reply`

- **Server → Client**: `Custom Event Message`
- **Server → Client**: `Specific IO Cancel Request`

## Message Flow Summary

| Direction      | Message Type             | Purpose                          |
|----------------|--------------------------|----------------------------------|
| Server → Client| Capabilities Request     | Request server capabilities      |
| Client → Server| Capabilities Reply       | Respond with capabilities        |
| Server → Client| CreateFile Request       | Request to create a file         |
| Client → Server| CreateFile Reply         | Confirm file creation            |
| Server → Client| Read Request             | Request to read data            |
| Client → Server| Read Reply               | Return read data                |
| Server → Client| Write Request            | Request to write data           |
| Client → Server| Write Reply              | Confirm write operation         |
| Server → Client| IoControl Request       | Request device control operation|
| Client → Server| IoControl Reply          | Confirm control operation       |
| Server → Client| Custom Event Message     | Send custom event notification  |
| Server → Client| Specific IO Cancel Request | Request to cancel specific I/O |

The diagram shows a clear request-reply pattern for most operations, indicating a synchronous communication
