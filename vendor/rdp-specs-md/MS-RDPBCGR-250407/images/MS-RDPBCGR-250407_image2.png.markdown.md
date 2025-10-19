# GCC Conference Create Request Structure

This diagram illustrates the structure of a GCC Conference Create Request, showing its hierarchical components and data groupings.

## Overall Request Structure

The entire request is labeled as **GCC Conference Create Request** and is composed of several distinct sections:

- **Connect Initial Fields**: A group of dotted lines at the top, indicating initial connection fields.
- **Conference Create Request Fields**: A group of dotted lines within the main request body, representing the core fields for creating a conference.
- **Connect Initial User Data**: A large section encompassing the user data blocks, indicating initial user-related information.

## Conference Create Request Fields

This section contains the core fields required to create a conference. It is represented by dotted lines and is grouped under the label "Conference Create Request Fields".

## Conference Create Request User Data

This is a nested section within the main request, containing multiple client data blocks:

- **Client Data Block 1**
- **Client Data Block 2**
- **...** (ellipsis indicating additional blocks)
- **Client Data Block N**

These blocks are grouped under the label "Conference Create Request User Data", suggesting they represent user-specific data blocks that can be multiple in number.

## Relationships and Groupings

- The **Connect Initial Fields** are positioned above the main request body.
- The **Conference Create Request Fields** are located within the main request body, above the user data blocks.
- The **Connect Initial User Data** encompasses all the client data blocks, indicating that these blocks constitute the user data portion of the request.

The diagram uses curly braces to visually group related components, clearly delineating the structure of the GCC Conference Create Request.
