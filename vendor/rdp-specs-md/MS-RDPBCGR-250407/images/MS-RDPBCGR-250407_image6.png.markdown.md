# Data Compression Algorithm Flowchart

This flowchart outlines a data compression algorithm that processes source data (`SrcData`) by searching for matches in a history buffer and encoding either matches (as copy-tuples) or literals. The algorithm manages flags and output buffers to efficiently compress data.

## Algorithm Overview

The algorithm begins by initializing flags and then iteratively processes the source data by:
1. Attempting to fit the source data into a history buffer.
2. Searching for matches in the history buffer.
3. Encoding matches as copy-tuples or literals.
4. Managing output buffer space and flushing when necessary.
5. Repeating until all source data is processed.

## Key Components

- **Start/End Points**: `Start Compress Data` and `Finished Compress Data`.
- **Flags**: A variable that accumulates compression flags (`PACKET_COMPR_TYPE`, `PACKET_AT_FRONT`, `PACKET_FLUSHED`).
- **History Buffer**: A buffer that stores previously processed data for match searching.
- **Output Buffer**: A buffer that accumulates encoded data before sending.

## Flowchart Structure

### Initialization
- **Start Compress Data**
  - Set `Flags = PACKET_COMPR_TYPE`
  - Check if `SrcData fits into HistoryBuffer?`
    - **N (No)**: Set `HistoryOffset = 0` and add `PACKET_AT_FRONT` to `Flags`.
    - **Y (Yes)**: Copy `SrcData` to `HistoryBuffer` at `HistoryOffset`, advance `HistoryOffset` by size of `SrcData`.

### Main Compression Loop

1. **Check History Pointer**
   - Check if `HistoryPtr < HistoryOffset?`
     - **N (No)**: Advance `HistoryPtr` by size of match and continue.
     - **Y (Yes)**: Search for a match of at least 3 bytes from the start of `HistoryBuffer` to `(HistoryPtr - 1)` for data immediately following `HistoryPtr`.

2. **Match Found?**
   - Check if `Match found in HistoryBuffer?`
     - **Y (Yes)**: Create an encoded copy-tuple describing the match.
       - Check if `Encoded copy-tuple fits into OutputBuffer?`
         - **Y (Yes)**: Add encoded copy-tuple to `OutputBuffer`.
         - **N (No)**: Add `PACKET_FLUSHED` to `Flags`, send `SrcData`, and loop back.
     - **N (No)**
