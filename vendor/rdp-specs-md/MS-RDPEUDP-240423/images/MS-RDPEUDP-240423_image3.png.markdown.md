# FEC Encoder Block Diagram

This diagram illustrates the input and output structure of a Forward Error Correction (FEC) encoder.

## Input Data

The FEC encoder receives three input streams, labeled as follows:

- `S₈`
- `S₂`
- `S₁`

These inputs are shown as separate rectangular blocks on the left side of the diagram.

## Encoder Module

- **Component**: `FecEncoder`
- **Function**: Processes the input streams to generate the output.
- **Position**: Central block in the diagram, receiving inputs and producing outputs.

## Output Data

The encoder produces four output streams, labeled as follows:

- `F₁-₃`
- `S₈`
- `S₂`
- `S₁`

These outputs are shown as separate rectangular blocks on the right side of the diagram.

## Data Flow

The diagram depicts a unidirectional flow:

1. The three input streams (`S₈`, `S₂`, `S₁`) are fed into the `FecEncoder`.
2. The encoder processes these inputs.
3. The result is four output streams (`F₁-₃`, `S₈`, `S₂`, `S₁`).

The output includes the original streams (`S₈`, `S₂`, `S₁`), indicating that they are preserved, along with a new stream (`F₁-₃`) which is likely the FEC parity data.

## Summary Table

| Component      | Type     | Description                     |
|----------------|----------|---------------------------------|
| `S₈`           | Input    | First input stream              |
| `S₂`           | Input    | Second input stream             |
| `S₁`           | Input    | Third input stream              |
| `FecEncoder`   | Processor| Central processing unit         |
| `F₁-₃`         | Output   | FEC parity or redundant data   |
| `S₈`           | Output   | Preserved original stream      |
| `S₂`           | Output   | Preserved original stream      |
| `S₁`           | Output   | Preserved original stream      |

This structure is typical for FEC systems where original data is transmitted along with redundant parity data to enable error recovery at the receiver.
