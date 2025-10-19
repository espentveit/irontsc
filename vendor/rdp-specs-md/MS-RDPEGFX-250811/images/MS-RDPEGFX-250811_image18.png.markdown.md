# Reverse Filter for Color Conversion

The following reverse filter must be applied to $\tilde{U}_{444}(2x,2y)$ and $\tilde{V}_{444}(2x,2y)$ prior to color conversion.

## Filter Equations

The filter is defined by two equations that transform the input chrominance components $ \tilde{U}_{444} $ and $ \tilde{V}_{444} $ into the output components $ U_{444} $ and $ V_{444} $.

### U Component Equation

$$
U_{444}(2x,2y) = \tilde{U}_{444}(2x,2y) \cdot 4 - U_{444}(2x + 1,2y) - U_{444}(2x,2y + 1) - U_{444}(2x + 1,2y + 1)
$$

### V Component Equation

$$
V_{444}(2x,2y) = \tilde{V}_{444}(2x,2y) \cdot 4 - V_{444}(2x + 1,2y) - V_{444}(2x,2y + 1) - V_{444}(2x + 1,2y + 1)
$$

## Key Observations

- The filter operates on pixel coordinates that are even multiples of 2 (i.e., $2x, 2y$), indicating it is applied to a subsampled or downsampled chrominance plane.
- The filter is applied to the *interpolated* or *upsampled* chrominance components $ \tilde{U}_{444} $ and $ \tilde{V}_{444} $, which are typically obtained from a lower-resolution chrominance plane.
- The filter uses a 3x3 neighborhood centered at $(2x, 2y)$, including the center pixel and its 8 neighbors, but only the 4 neighbors that are offset by 1 in x or y direction.
- The filter is applied prior to color conversion, suggesting it is part of a chrominance resampling or reconstruction process, possibly during the conversion from YUV or YCbCr to RGB.
- The coefficients are 4 (for the center pixel) and -
