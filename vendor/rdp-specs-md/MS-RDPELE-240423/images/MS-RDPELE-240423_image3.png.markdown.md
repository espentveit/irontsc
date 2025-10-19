# Sequence Diagram: Remote Desktop Client and Terminal Server License Exchange

This sequence diagram illustrates the interaction between a Remote Desktop Client and a Terminal Server to obtain and validate a new license.

## Participants

- **Remote Desktop Client**: The client application requesting a license.
- **Terminal Server**: The server providing and validating licenses.

## Message Flow

The interaction proceeds in the following sequence:

1. **License Request**
   - Direction: Terminal Server → Remote Desktop Client
   - Description: The Terminal Server initiates a license request to the client.

2. **New License Request**
   - Direction: Remote Desktop Client → Terminal Server
   - Description: The client responds to the initial request by sending a new license request to the server.

3. **Platform Challenge**
   - Direction: Terminal Server → Remote Desktop Client
   - Description: The server sends a platform challenge to the client, likely to verify the client's environment or identity.

4. **Platform Challenge Response**
   - Direction: Remote Desktop Client → Terminal Server
   - Description: The client responds to the platform challenge sent by the server.

5. **New License**
   - Direction: Terminal Server → Remote Desktop Client
   - Description: The server sends the newly issued license to the client.

## Summary Table

| Step | Message                  | Direction                 | Description                                      |
|------|--------------------------|---------------------------|--------------------------------------------------|
| 1    | License Request          | Terminal Server → Client  | Initial request for license from the server.     |
| 2    | New License Request      | Client → Terminal Server  | Client sends a new license request to the server.|
| 3    | Platform Challenge       | Terminal Server → Client  | Server sends a challenge to verify the client.   |
| 4    | Platform Challenge Response | Client → Terminal Server | Client responds to the platform challenge.       |
| 5    | New License              | Terminal Server → Client  | Server grants the new license to the client.     |
