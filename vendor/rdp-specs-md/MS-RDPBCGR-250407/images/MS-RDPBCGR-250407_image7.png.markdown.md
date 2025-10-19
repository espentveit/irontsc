# Multifragment Reassembly Buffer Diagram

This diagram illustrates the process of reassembling fragmented data into a single buffer, showing three sequential Fast-Path Updates that contribute to the final buffer.

## Overall Structure

- **Multifragment Reassembly Buffer**: A large buffer that accumulates data from multiple fragments.
  - Total size: **46,650 bytes**
  - Divided into three distinct sections, each corresponding to a Fast-Path Update.

## Fast-Path Updates

Three Fast-Path Updates are shown, each adding a portion of the data to the buffer:

1. **First Fragment Update (Position 1)**
   - **Label**: "Fast-Path Update containing FIRST fragment"
   - **Size**: 15,492 bytes
   - **Contents**: 
     - Header
     - Update Data

2. **Next Fragment Update (Position 2)**
   - **Label**: "Fast-Path Update containing NEXT fragment"
   - **Size**: 15,492 bytes
   - **Contents**: 
     - Header
     - Update Data

3. **Last Fragment Update (Position 3)**
   - **Label**: "Fast-Path Update containing LAST fragment"
   - **Size**: 9,180 bytes
   - **Contents**: 
     - Header
     - Update Data

## Buffer Composition

The final Multifragment Reassembly Buffer is composed of the data from the three Fast-Path Updates, arranged sequentially:

- **Section 1**: Data from the FIRST fragment (15,492 bytes)
- **Section 2**: Data from the NEXT fragment (15,492 bytes)
- **Section 3**: Data from the LAST fragment (9,180 bytes)

The diagram visually represents the buffer as a single continuous block, with the three updates contributing to its total size of 46,650 bytes. Each update is shown with its own header and update data portion, indicating a structured format for each fragment.
