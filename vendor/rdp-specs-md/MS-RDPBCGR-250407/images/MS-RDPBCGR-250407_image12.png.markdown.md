# Data Packet Structure Diagram

This diagram illustrates the structure of a data packet, showing the different fields and their encryption status.

## Packet Fields

The packet is divided into five labeled sections:

- Fast-Path Header
- Length
- FIPS Information
- MAC Signature
- Data

## Encryption Status Legend

The legend indicates the encryption status of the data segments:

- **Unencrypted Data**: Represented by a white square (`□`)
- **Encrypted Data**: Represented by a gray square (`■`)

## Visual Representation

The diagram visually represents the packet structure with the following encryption status:

| Field              | Encryption Status |
|--------------------|-------------------|
| Fast-Path Header   | Unencrypted Data  |
| Length             | Unencrypted Data  |
| FIPS Information   | Unencrypted Data  |
| MAC Signature      | Unencrypted Data  |
| Data               | Encrypted Data    |

Note: The "Data" field is shaded gray, indicating it is encrypted, while all other fields are unencrypted.
