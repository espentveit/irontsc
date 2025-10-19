# Clipboard Data Transfer Sequence Diagram

This diagram illustrates the sequence of Protocol Data Units (PDUs) exchanged between a Shared Clipboard Owner and a Local Clipboard Owner during clipboard data transfer operations.

## Participants

- **Shared Clipboard Owner**: The source of the clipboard data.
- **Local Clipboard Owner**: The destination receiving the clipboard data.

## Sequence Overview

The diagram shows a two-phase process: a "Copy Sequence" and a "Paste Sequence" (which includes two sub-sequences).

### Phase 1: Copy Sequence

This phase establishes the format list and optionally locks the clipboard data.

1. **Format List PDU**  
   - Direction: Shared Clipboard Owner → Local Clipboard Owner  
   - Purpose: Requests the list of available clipboard formats.

2. **Format List Response PDU**  
   - Direction: Local Clipboard Owner → Shared Clipboard Owner  
   - Purpose: Responds with the list of available clipboard formats.

3. **Lock Clipboard Data PDU (Optional)**  
   - Direction: Local Clipboard Owner → Shared Clipboard Owner  
   - Purpose: Optional message to lock the clipboard data for exclusive access.

### Phase 2: Paste Sequence

This phase involves transferring the actual clipboard data.

#### Sub-Phase A: Paste Sequence for Generic, Palette, Metafile, and File List Data

1. **Format Data Request PDU**  
   - Direction: Local Clipboard Owner → Shared Clipboard Owner  
   - Purpose: Requests the data for specific clipboard formats.

2. **Format Data Response PDU**  
   - Direction: Shared Clipboard Owner → Local Clipboard Owner  
   - Purpose: Responds with the requested clipboard data.

#### Sub-Phase B: Paste Sequence for File Stream Data

1. **File Contents Request PDU**  
   - Direction: Local Clipboard Owner → Shared Clipboard Owner  
   - Purpose: Requests the contents of a file stream.

2. **File Contents Response PDU**  
   - Direction: Shared Clipboard Owner → Local Clipboard Owner  
   - Purpose: Responds with the requested file stream data.

### Final Step

3. **Unlock Clipboard Data PDU (Optional)**  
   - Direction: Local Clipboard Owner → Shared Clipboard Owner  
   - Purpose: Optional message to unlock the clipboard data after transfer.

## Notes

- The diagram uses arrows to indicate the direction of data flow.
- Optional steps are explicitly marked as "(Optional)".
- The diagram groups related PDUs under descriptive labels: "Copy Sequence" and "Paste Sequence" with further subdivisions for different data types.
