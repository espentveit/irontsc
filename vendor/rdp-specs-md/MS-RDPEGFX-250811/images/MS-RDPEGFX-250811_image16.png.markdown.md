# YUV420 Color Space Representation

This diagram illustrates the YUV420 color space format, which separates an image into luminance (Y) and chrominance (U, V) components with a specific subsampling pattern.

## Main View (Y Component)

- **Y (B1)**: The luminance component, which contains the brightness information of the image.
  - This component is full-resolution, meaning it has the same dimensions as the original image.
  - Labeled as `B1`.

## Chroma420 View (U and V Components)

- **Chroma420**: This section represents the chrominance components, which are subsampled to reduce data size while preserving color information.
  - **U (B4)** and **V (B5)**: These components are combined into a single block in the auxiliary view.
    - The dashed line indicates that U and V are stored together in this block.
    - Labeled as `B4` and `B5` respectively.
  - **U (B6)**: A separate U component block.
    - Labeled as `B6`.
  - **V (B7)**: A separate V component block.
    - Labeled as `B7`.

## Component Mapping

The diagram shows how the YUV420 format is structured:

| Component | Label | Resolution | Description |
|-----------|-------|------------|-------------|
| Y         | B1    | Full       | Luminance (brightness) |
| U         | B2    | Half       | Chrominance (blue difference) |
| V         | B3    | Half       | Chrominance (red difference) |
| U         | B4    | Quarter    | Chrominance (blue difference) |
| V         | B5    | Quarter    | Chrominance (red difference) |
| U         | B6    | Quarter    | Chrominance (blue difference) |
| V         | B7    | Quarter    | Chrominance (red difference) |

## Notes

- The "Main View" contains the Y component (B1) and the U and V components (B2, B3) at half resolution.
- The "Auxiliary View" contains the U and V components (B4, B5, B6, B7) at quarter resolution.
- The diagram uses labels B1 through B7 to identify each component block.
- The dashed line in the auxiliary view indicates that U and
