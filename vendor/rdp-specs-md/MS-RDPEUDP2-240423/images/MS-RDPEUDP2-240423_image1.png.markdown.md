# Communication Diagram: Endpoint A and Endpoint B

This diagram illustrates a communication model between two endpoints, Endpoint A and Endpoint B, showing the flow of data and acknowledgments between them.

## Endpoints

The diagram contains two main logical groupings:

- **Endpoint A**: Contains a Sender and a Receiver.
- **Endpoint B**: Contains a Sender and a Receiver.

Each endpoint is represented as a rounded rectangle, indicating a distinct logical unit.

## Communication Channels

There are two distinct communication paths shown, each with a data flow and an acknowledgment flow.

### Path 1: Endpoint A Sender ↔ Endpoint B Receiver

- **Data Flow**: Solid arrow from Endpoint A's Sender to Endpoint B's Receiver, labeled "Data".
- **Acknowledgment Flow**: Dashed arrow from Endpoint B's Receiver back to Endpoint A's Sender, labeled "Acknowledgments".

### Path 2: Endpoint A Receiver ↔ Endpoint B Sender

- **Data Flow**: Solid arrow from Endpoint A's Receiver to Endpoint B's Sender, labeled "Data".
- **Acknowledgment Flow**: Dashed arrow from Endpoint B's Sender back to Endpoint A's Receiver, labeled "Acknowledgments".

## Summary Table

| Component       | Endpoint A | Endpoint B |
|-----------------|------------|------------|
| Sender          | Present    | Present    |
| Receiver        | Present    | Present    |
| Data Direction  | → (to B)   | ← (from A) |
| Acknowledgment Direction | ← (from B) | → (to A) |

This structure suggests a bidirectional communication model where each endpoint can act as both a sender and a receiver, and acknowledgments are sent back along the same logical path as the data.
