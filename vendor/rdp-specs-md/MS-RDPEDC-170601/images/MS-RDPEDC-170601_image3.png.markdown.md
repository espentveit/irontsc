# Sequence Diagram: Server-Client Interaction for Surface Composition

This sequence diagram illustrates the interaction between a Server and a Client, focusing on the lifecycle of logical surfaces and the role of surface redirection in a composition system.

## Overview

The diagram depicts a sequence of messages exchanged between the Server and Client, organized into three distinct phases or cycles:

1. **Redir Surface Cycle**
2. **Logical Surface Cycle**
3. **Composition Cycle**

These cycles are grouped by a large right curly brace, indicating they are part of the overall composition process.

## Message Flow

The following messages are exchanged in sequence:

- **COMPDESKTOGGLE - Composition_ON** → Initiates composition mode.
- **COMPDESK_LSURFACE - Create - hLSurf1** → Creates a logical surface.
- **COMPDESK_SURFOBJ - Create - hSurf1** → Creates a surface object associated with hLSurf1.
- **COMPDESK_ASSOC_Attach - hLSurf1, hSurf1** → Attaches the surface object to the logical surface.
- **COMPDESK_ASSOC_Detach - hLSurf1, hSurf1** → Detaches the surface object from the logical surface.
- **COMPDESK_SURFOBJ - Delete - hSurf1** → Deletes the surface object.
- **COMPDESK_ASSOC_Attach - hLSurf1, hSurf2** → Attaches a new surface object (hSurf2) to the same logical surface.
- **COMPDESK_ASSOC_Detach - hLSurf1, hSurf2** → Detaches the new surface object.
- **COMPDESK_SURFOBJ - Delete - hLSurf2** → Deletes the surface object (note: likely a typo, should be hSurf2).
- **COMPDESK_LSURFACE - Delete - hLSurf1** → Deletes the logical surface.
- **COMPDESKTOGGLE - Composition_OFF** → Deactivates composition mode.

## Cycles Breakdown

### Redir Surface Cycle

This cycle includes the initial creation and deletion of surface objects (`hSurf1` and `hSurf2`) and their attachment/detachment from the logical surface (`hLSurf1`). It is enclosed in a curly brace labeled "Redir Surface Cycle".

### Logical Surface Cycle

This cycle involves the creation and deletion of the logical surface (`hLSurf1`) and the attachment/detachment of surface objects to it
