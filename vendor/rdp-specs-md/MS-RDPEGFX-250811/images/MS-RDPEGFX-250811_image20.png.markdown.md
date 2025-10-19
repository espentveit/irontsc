# YUV420 Color Space Representation

This diagram illustrates the YUV420 color space format, which separates luminance (Y) and chrominance (U, V) components with a specific subsampling pattern.

## Main View (Y Component)

The main view represents the luminance component (Y) and the chrominance components (U, V) at full resolution.

- **Y (B1)**: Full-resolution luminance component.
- **U (B2)**: Chrominance component U at half the resolution of Y.
- **V (B3)**: Chrominance component V at half the resolution of Y.

## Auxiliary View (Chroma420)

The auxiliary view shows the chrominance components (U, V) with a 4:2:0 subsampling pattern, meaning the chrominance is sampled at half the horizontal and vertical resolution of the luminance.

- **U (B4)**: Chrominance component U at half the resolution of Y.
- **V (B5)**: Chrominance component V at half the resolution of Y.
- **U (B6)**: Chrominance component U at quarter the resolution of Y.
- **V (B7)**: Chrominance component V at quarter the resolution of Y.
- **U (B8)**: Chrominance component U at quarter the resolution of Y.
- **V (B9)**: Chrominance component V at quarter the resolution of Y.

## Structure Summary

| Component | Resolution Relative to Y | Block ID |
|-----------|--------------------------|----------|
| Y         | Full resolution          | B1       |
| U         | Half resolution          | B2, B4, B6, B8 |
| V         | Half resolution          | B3, B5, B7, B9 |

The diagram visually represents how the YUV420 format organizes data for efficient storage and transmission by separating the luminance and chrominance components and subsampling the chrominance.
