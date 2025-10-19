# License Validation Flowchart

This flowchart illustrates the process for validating a client's license and handling various scenarios related to license status, upgrades, and server communication.

## Main Flow

The process begins with the client sending a `Client Info Packet` to the server, which then responds with a `Server License Request Packet`.

### Server Certificate Validation

- **Decision**: Is the Server Cert Valid?
  - **NO**: Flow leads to `C → S Error Abort`
  - **YES**: Proceed to check license availability

### License Availability Check

- **Decision**: Is there a valid license in the store?
  - **NO**: Client sends a `Client New License Request`
    - This leads to a `Platform Challenge` and `Platform Challenge Response` exchange.
    - Then checks if the `Grace Period` has expired.
      - **YES**: Flow leads to `S → C Error Abort`
      - **NO**: Proceeds to check if the license server (LS) could be contacted.
        - **YES**: Checks if the license was issued.
          - **YES**: Sends `New License Packet`
          - **NO**: Flow leads to `S → C Error Abort`
        - **NO**: Flow leads to `S → C Error Abort`
  - **YES**: Client sends `Client License Info`
    - **Decision**: Is it a valid license?
      - **NO**: Flow leads to `S → C Success`
      - **YES**: **Decision**: Is it a valid temporary license?
        - **NO**: **Decision**: Does it need an upgrade?
          - **NO**: Flow leads to `S → C Success`
          - **YES**: Flow leads to `S → C Platform Challenge`
            - Client sends `Platform Challenge Response`
            - **Decision**: Could the LS be contacted?
              - **NO**: Flow leads to `S → C Error Abort`
              - **YES**: **Decision**: Was the License Upgraded?
                - **YES**: Flow leads to `S → C Upgrade License Packet`
                - **NO**: **Decision**: Is the Client License Still Valid?
                  - **YES**: Flow leads to `S → C Success`
                  - **NO**: Flow leads to `S → C Error Abort`
        - **YES**: Flow leads to `S → C Platform Challenge`
          - Client sends `Platform Challenge Response`
          - **Decision**: Could the LS be contacted?
            - **NO**: Flow leads to `S → C Error Abort`
