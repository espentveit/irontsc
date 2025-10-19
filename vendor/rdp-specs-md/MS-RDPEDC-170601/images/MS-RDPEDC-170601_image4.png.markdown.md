# Sequence Diagram: Logical Surface Cycle

This sequence diagram illustrates the interaction between a Server and a Client during a "Logical Surface Cycle," which encompasses the creation, usage, and deletion of redirected surfaces.

## Overview

The diagram shows a vertical timeline of messages exchanged between the Server (left) and the Client (right). The entire process is grouped into three main phases:

1. **Redir Surface Creation**
2. **Drawing Cycle**
3. **Redir Surface Deletion**

Each phase is visually grouped with a curly brace and labeled accordingly.

## Phase 1: Redir Surface Creation

This phase involves the creation of a redirected surface and its association.

- `COMPDESK_LSURFACE - Create - hLSurf1`
  - The Server sends a message to create a logical surface, returning handle `hLSurf1`.
- `COMPDESK_SURFOBJ - Create - hSurf1`
  - The Server creates a surface object, returning handle `hSurf1`.
- `COMPDESK_ASSOC-Attach - hLSurf1, hSurf1`
  - The Server attaches the logical surface (`hLSurf1`) to the surface object (`hSurf1`).

## Phase 2: Drawing Cycle

This phase involves rendering operations and a final flush.

- `COMPDESK_SWITCH_SURFOBJ - hSurf1`
  - The Server switches to the surface object `hSurf1` for drawing.
- `Drawing Orders (not part of this document)`
  - This represents drawing commands, which are noted as outside the scope of this document.
- `COMPDESK_FLUSH_COMPOSEONCE`
  - The Server flushes and composes the drawing operations.

## Phase 3: Redir Surface Deletion

This phase involves detaching and deleting the surface components.

- `COMPDESK_ASSOC-Detach - hLSurf1, hSurf1`
  - The Server detaches the logical surface from the surface object.
- `COMPDESK_SURFOBJ - Delete - hSurf1`
  - The Server deletes the surface object `hSurf1`.
- `COMPDESK_LSURFACE - Delete - hLSurf1`
  - The Server deletes the logical surface `hLSurf1`.

## Message Flow Summary

| Message | Direction | Description |
|---------|-----------|-------------|
| `COMPDESK_LSURFACE - Create - hLSurf1` | Server → Client | Creates a logical surface |
