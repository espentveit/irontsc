# Image Processing Pipeline Diagram

This diagram illustrates a multi-stage image encoding process, likely for a compression algorithm. The flow is sequential, with some feedback loops for reference data.

## Main Processing Flow

The primary data path proceeds as follows:

1.  **Input image/region** → Enters the pipeline.
2.  **Color conversion (RGB to YCbCr)** → Converts the input color space.
3.  **DWT** → Applies Discrete Wavelet Transform.
4.  **Quantization and linearization** → Reduces precision and transforms data.
5.  **Sub-band diffing** → Performs differential encoding across wavelet sub-bands.
6.  **Progressive entropy encoding** → Encodes the data progressively using entropy coding.
7.  **Encoded tiles** → Output of the encoding process.

## Feedback and Reference Mechanism

-   **Reference bits** → This block receives feedback from the "Progressive entropy encoding" stage via a dashed line.
-   The "Reference bits" block feeds back into the "Sub-band diffing" stage via a dashed line, suggesting that reference data is used to assist in the differential encoding process.

## Key Components and Relationships

| Stage | Description | Input | Output | Feedback |
| :--- | :--- | :--- | :--- | :--- |
| Input | Starting point | Input image/region | - | - |
| Color conversion | Converts RGB to YCbCr | Input image/region | Data for DWT | - |
| DWT | Discrete Wavelet Transform | Color-converted data | Data for Quantization | - |
| Quantization and linearization | Reduces precision and linearizes data | DWT output | Data for Sub-band diffing | - |
| Sub-band diffing | Differential encoding across sub-bands | Quantized data | Data for Progressive entropy encoding | Reference bits (feedback) |
| Progressive entropy encoding | Progressive entropy coding | Sub-band diffing output | Encoded tiles | Reference bits (feedback) |
| Reference bits | Stores reference data for diffing | Progressive entropy encoding (dashed) | Feedback to Sub-band diffing | - |

The dashed lines indicate a feedback loop, where "Reference bits" are generated from the "Progressive entropy encoding" stage and used to inform the "Sub-band diffing" stage. This is typical in predictive or differential coding schemes.
