# Packet Header Structure

The image displays a diagram of a packet header structure, with different fields and a legend indicating which parts are encrypted or unencrypted.

## Header Fields

The header is divided into four labeled sections:

- Fast-Path Header
- Length
- MAC Signature
- Data

## Legend

The legend indicates the encryption status of different parts of the packet:

- **White box** = Unencrypted Data
- **Gray box** = Encrypted Data

## Encryption Status

Based on the diagram:

- The "Fast-Path Header", "Length", and "MAC Signature" fields are shown as white boxes, indicating they are **unencrypted**.
- The "Data" field is shown as a gray box, indicating it is **encrypted**.

This structure suggests that while the control information (header, length, MAC) is transmitted in plaintext, the actual payload (Data) is encrypted for security.
