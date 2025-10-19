# SSL Record Structure Diagram

This diagram illustrates the structure of an SSL record, showing how different components are organized and whether they are encrypted or unencrypted.

## Legend

- **White box** = Unencrypted Data
- **Gray box** = Encrypted Data

## Record Structure

The SSL record is composed of four sequential components:

| Component         | Description                 | Encryption Status |
|-------------------|-----------------------------|-------------------|
| SSL Record Header | Header for the SSL record   | Unencrypted       |
| Fast-Path Header  | Fast-path specific header   | Encrypted         |
| Length            | Length of the data field    | Encrypted         |
| Data              | Actual payload/data content | Encrypted         |

## Notes

- The diagram indicates that the `SSL Record Header` is unencrypted.
- The `Fast-Path Header`, `Length`, and `Data` fields are all encrypted.
- The components are arranged sequentially from left to right.
