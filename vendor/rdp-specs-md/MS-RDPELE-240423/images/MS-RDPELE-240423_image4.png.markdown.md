# Sequence Diagram: Remote Desktop Client and Terminal Server License Exchange

This sequence diagram illustrates the interaction between a Remote Desktop Client and a Terminal Server during a license negotiation process.

## Participants

- **Remote Desktop Client**: Initiates the license request and responds to platform challenges.
- **Terminal Server**: Sends license requests, challenges, and upgrade licenses.

## Message Flow

The diagram shows a sequence of five messages exchanged between the two participants:

1. **License Request**
   - Direction: Terminal Server → Remote Desktop Client
   - Description: The Terminal Server sends a license request to the Remote Desktop Client.

2. **License Info**
   - Direction: Remote Desktop Client → Terminal Server
   - Description: The Remote Desktop Client sends license information back to the Terminal Server.

3. **Platform Challenge**
   - Direction: Terminal Server → Remote Desktop Client
   - Description: The Terminal Server sends a platform challenge to the Remote Desktop Client.

4. **Platform Challenge Response**
   - Direction: Remote Desktop Client → Terminal Server
   - Description: The Remote Desktop Client responds to the platform challenge.

5. **Upgrade License**
   - Direction: Terminal Server → Remote Desktop Client
   - Description: The Terminal Server sends an upgrade license to the Remote Desktop Client.

## Diagram Structure

- The diagram uses vertical dashed lines to represent the lifelines of each participant.
- Horizontal arrows indicate messages with their direction and label.
- All messages are sent in a top-to-bottom sequence, representing the chronological order of events.

This interaction is typical in software licensing systems where a client must authenticate its platform and then receive an appropriate license, potentially including an upgrade.
