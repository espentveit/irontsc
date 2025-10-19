# Sequence Diagram: Server-Client Interaction

This sequence diagram illustrates a communication flow between a Server and a Client, showing the exchange of messages in a specific order.

## Participants

- **Server**: Initiates the communication and responds to the Client.
- **Client**: Responds to the Server and sends acknowledgments.

## Message Flow

The diagram depicts a four-step interaction:

1. **Exchange Capabilities Request**
   - Sent from the Server to the Client.
   - Purpose: To initiate a capability exchange.

2. **Exchange Capabilities Respond message**
   - Sent from the Client to the Server.
   - Purpose: To respond to the capability request.

3. **Channel Created message**
   - Sent from the Server to the Client.
   - Purpose: To notify the Client that a channel has been created.

4. **Channel Created message**
   - Sent from the Client to the Server.
   - Purpose: To acknowledge receipt of the channel creation notification.

## Message Exchange Summary

| Step | Message Name                      | Direction       | Sender    | Receiver  |
|------|-----------------------------------|-----------------|-----------|-----------|
| 1    | Exchange Capabilities Request     | Server → Client | Server    | Client    |
| 2    | Exchange Capabilities Respond     | Client → Server | Client    | Server    |
| 3    | Channel Created message           | Server → Client | Server    | Client    |
| 4    | Channel Created message           | Client → Server | Client    | Server    |

## Diagram Structure

- The diagram uses vertical lifelines to represent the Server and Client.
- Horizontal arrows indicate messages exchanged between the two participants.
- The arrows show the direction of message flow and the sequence of events.
