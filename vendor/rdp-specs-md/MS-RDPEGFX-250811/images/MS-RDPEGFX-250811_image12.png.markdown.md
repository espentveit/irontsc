# Decoding Process Flowchart

This diagram illustrates the decoding process for a video or image tile, likely within a video compression standard such as H.264/AVC or HEVC, showing how an encoded tile is progressively decoded and reconstructed.

## Main Flow

The process begins with an input and proceeds through several stages to produce a decoded output.

- **Input**: `Encoded tile`
  - Enters the `Progressive entropy decode` block.

- **Progressive entropy decode**
  - Performs entropy decoding on the encoded tile.
  - Receives feedback from the `Persistent progressive state` block.
  - Outputs to the `Add` block.
  - Also has a feedback connection to the `Current frame` block (dashed line).

- **Add**
  - Combines the output from `Progressive entropy decode` with the `Current frame`.
  - Outputs to the `Inverse DWT` block.

- **Inverse DWT**
  - Performs inverse Discrete Wavelet Transform.
  - Outputs to the `Color conversion (YCbCr to RGB)` block.

- **Color conversion (YCbCr to RGB)**
  - Converts the decoded chroma and luma components to RGB color space.
  - Outputs the final result: `Decoded image/region`.

## Feedback Loops and State Management

The diagram includes feedback mechanisms to support progressive decoding.

- **Persistent progressive state**
  - Maintains state information across successive decoding operations.
  - Provides feedback to the `Progressive entropy decode` block (dashed line).
  - Receives feedback from the `Progressive entropy decode` block (dashed line).

- **Current frame**
  - Represents the reconstructed frame or region being built progressively.
  - Receives input from the `Progressive entropy decode` block (dashed line).
  - Provides input to the `Add` block (dashed line).
  - Also receives feedback from the `Add` block (dashed line).

## Summary of Blocks and Connections

| Block Name | Description | Input(s) | Output(s) |
|------------|-------------|----------|-----------|
| Encoded tile | Input source | — | Progressive entropy decode |
| Progressive entropy decode | Entropy decoding stage | Encoded tile, Persistent progressive state | Add |
| Persistent progressive state | Maintains decoding state | Progressive entropy decode | Progressive entropy decode |
| Add | Combines decoded data with current frame | Progressive entropy decode, Current frame | Inverse DWT |
| Current frame | Re
