# Communication Diagram: Endpoint A and Endpoint B

This diagram illustrates a communication model between two endpoints, Endpoint A and Endpoint B, showing the flow of data and acknowledgments between their respective sender and receiver components.

## Endpoint A

- **Sender**
  - Sends data to Endpoint B's Receiver.
  - Receives acknowledgments from Endpoint B's Receiver.
- **Receiver**
  - Receives data from Endpoint B's Sender.
  - Sends acknowledgments to Endpoint B's Sender.

## Endpoint B

- **Receiver**
  - Receives data from Endpoint A's Sender.
  - Sends acknowledgments to Endpoint A's Sender.
- **Sender**
  - Sends data to Endpoint A's Receiver.
  - Receives acknowledgments from Endpoint A's Receiver.

## Communication Flow

The diagram depicts bidirectional communication with the following flows:

| Direction        | From             | To               | Message Type       |
|------------------|------------------|------------------|--------------------|
| Data Flow 1      | Endpoint A Sender | Endpoint B Receiver | Data               |
| Acknowledgment 1 | Endpoint B Receiver | Endpoint A Sender | Acknowledgments    |
| Data Flow 2      | Endpoint B Sender | Endpoint A Receiver | Data               |
| Acknowledgment 2 | Endpoint A Receiver | Endpoint B Sender | Acknowledgments    |

- **Data** flows are represented by solid lines with solid arrowheads.
- **Acknowledgments** flow is represented by dashed lines with solid arrowheads.

This structure suggests a full-duplex communication model where each endpoint can send and receive data simultaneously, with each data transmission being acknowledged by the receiver.
