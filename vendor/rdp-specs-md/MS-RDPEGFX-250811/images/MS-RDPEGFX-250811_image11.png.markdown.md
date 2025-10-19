# Bitmap Decompression Flowchart

This flowchart outlines the process for decompressing a bitmap using the ClearCodec format, with special handling for glyph data.

## Start and Initial Setup

- **Start Decompress Bitmap**
  - The process begins here.
- **Read flags field from ClearCodec bitmap stream header**
  - Initial step to read the header flags.
- **Is "glyph index" flag set?**
  - Decision point to determine if glyph data is involved.
    - **Yes** → Proceed to read glyphIndex field.
    - **No** → Proceed to read composite payload header parameters.

## Glyph Index Handling

- **Read glyphIndex field**
  - If the "glyph index" flag is set, read the glyphIndex field.
- **Is "glyph hit" flag set?**
  - Decision point to determine if the glyph is a hit (i.e., should be copied).
    - **Yes** → Copy pixels from the Decompressor Glyph Storage position specified by the glyphIndex field to the output bitmap.
    - **No** → Proceed to read composite payload header parameters.

## Composite Payload Processing

- **Read composite payload header parameters**
  - Read the header parameters for the composite payload.
- **Is residual byte count > 0?**
  - Decision point for residual layer.
    - **Yes** → Decompress residual layer and write to output bitmap.
    - **No** → Proceed to check bands bytes.
- **Is bands bytes count > 0?**
  - Decision point for bands layer.
    - **Yes** → Decompress bands layer and write to output bitmap.
    - **No** → Proceed to check subcodec bytes.
- **Is subcodec byte count > 0?**
  - Decision point for subcodec layer.
    - **Yes** → Decompress subcodec layer and write to output bitmap.
    - **No** → Set alpha channel in output bitmap to fully opaque (0xFF).

## Final Steps

- **Is "glyph index" flag set?**
  - Decision point to determine if the glyph index should be used for storage.
    - **Yes** → Copy decompressed bitmap to the Decompressor Glyph Storage position specified by the glyphIndex field.
    - **No** → Proceed to finished state.
- **Finished Decompress Bitmap**
  - End of the decompression process.

## Notes

- The flowchart shows a branching structure with multiple decision points based on flag settings and byte counts.
- The process handles different layers
