# GCC Conference Create Response Structure

This diagram illustrates the hierarchical structure of a "GCC Conference Create Response" message, which is part of a larger "Connect Response" message.

## Overall Structure

The entire message is labeled as **GCC Conference Create Response** and is contained within the broader **Connect Response** message.

## Components of the GCC Conference Create Response

The GCC Conference Create Response contains two main sections:

### 1. Conference Create Response Fields
- A set of fields that constitute the core response data.
- Represented by dotted lines and grouped with a brace.
- These fields are part of the inner structure of the GCC Conference Create Response.

### 2. Conference Create Response User Data
- This section contains a sequence of server data blocks.
- It is enclosed within a gray box and labeled as "Conference Create Response User Data".
- The user data consists of multiple server data blocks, indicated as:
  - Server Data Block 1
  - Server Data Block 2
  - ...
  - Server Data Block N

## Relationship to Connect Response

The GCC Conference Create Response is nested within the larger Connect Response structure. It contributes to two parts of the Connect Response:

- **Connect Response Fields**: These are the top-level fields of the Connect Response, shown above the GCC Conference Create Response.
- **Connect Response User Data**: This section encompasses the entire GCC Conference Create Response, including both its fields and user data.

## Summary Table

| Component                        | Description                                                                 |
|---------------------------------|-----------------------------------------------------------------------------|
| GCC Conference Create Response  | Main container for the conference creation response.                        |
| Conference Create Response Fields | Core fields within the GCC Conference Create Response.                     |
| Conference Create Response User Data | Sequence of Server Data Blocks (1 to N) contained within the response.    |
| Connect Response Fields         | Top-level fields of the overall Connect Response message.                  |
| Connect Response User Data      | Contains the entire GCC Conference Create Response (fields + user data).   |

This structure suggests a layered protocol response where a specific operation (Conference Create) is embedded within a general connection response framework.
