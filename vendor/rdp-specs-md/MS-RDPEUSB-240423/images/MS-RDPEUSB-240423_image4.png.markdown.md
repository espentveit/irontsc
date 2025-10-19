# Sequence Diagram: Server-Client Interaction

This sequence diagram illustrates a series of message exchanges between a Server and a Client, detailing a communication protocol involving device queries, data transfers, and request cancellation.

## Participants

- **Server**: Initiates queries and requests, receives responses and completion messages.
- **Client**: Responds to queries, executes transfer requests, and sends cancellation requests.

## Message Flow

The diagram depicts a sequence of 9 messages exchanged between the Server and Client in a specific order:

1. **Query Device Text message**
   - Direction: Server → Client
   - Purpose: Server requests textual information about a device from the Client.

2. **Query Device Text Respond message**
   - Direction: Client → Server
   - Purpose: Client responds to the Server with the requested device text information.

3. **Transfer In Request message**
   - Direction: Server → Client
   - Purpose: Server requests the Client to initiate a data transfer.

4. **URB Completion message**
   - Direction: Client → Server
   - Purpose: Client notifies the Server that the previous transfer request has completed.

5. **Transfer In Request message**
   - Direction: Server → Client
   - Purpose: Server sends another data transfer request to the Client.

6. **Transfer In Request message**
   - Direction: Server → Client
   - Purpose: Server sends a third data transfer request to the Client.

7. **URB Completion message**
   - Direction: Client → Server
   - Purpose: Client notifies the Server that the second transfer request has completed.

8. **Cancel Request message**
   - Direction: Server → Client
   - Purpose: Server requests the Client to cancel an ongoing or pending operation.

9. **URB Completion message**
   - Direction: Client → Server
   - Purpose: Client notifies the Server that the cancellation request has been processed or the associated operation has completed.

## Summary Table

| Message Type                  | Direction     | Description                                 |
|------------------------------|---------------|---------------------------------------------|
| Query Device Text message    | Server → Client | Request for device text information         |
| Query Device Text Respond    | Client → Server | Response containing device text             |
| Transfer In Request message  | Server → Client | Request to initiate data transfer           |
| URB Completion message       | Client → Server | Notification that a transfer is complete    |
| Cancel Request message       | Server → Client | Request to cancel an ongoing operation      |
| URB Completion message       | Client → Server
