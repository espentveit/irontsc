# SSL Record Structure with Headers and Data

The image displays a diagram of an SSL record structure, segmented into distinct header and data components. The diagram also includes a legend indicating which parts are encrypted and which are unencrypted.

## Legend

- **White box** = Unencrypted Data
- **Gray box** = Encrypted Data

## Structure Breakdown

The SSL record is composed of the following sequential components:

| Component           | Description                                                                 |
|---------------------|-----------------------------------------------------------------------------|
| SSL Record Header   | The initial header of the SSL record. This section is shown as encrypted.  |
| TPKT Header         | The TPKT (Transaction Protocol Control Transport) header. Encrypted.       |
| X.224 Data Header   | The X.224 data header. Encrypted.                                          |
| MCS Header          | The MCS (Multiplexing Control Service) Header, which can be either a "Send Data Request" or "Send Data Indication". Encrypted. |
| Data                | The actual data payload. Encrypted.                                        |

## Observations

- All components in the SSL record structure shown in the diagram are encrypted, as indicated by the gray shading.
- The diagram does not show any unencrypted sections within the record.
- The structure is presented as a linear sequence of headers followed by the data payload.
- The MCS Header is explicitly labeled with its possible types: "Send Data Request" or "Send Data Indication".
