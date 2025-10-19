# Reverse Filter for Color Conversion

The following reverse filter must be applied to $\tilde{U}_{444}(2x, 2y)$ and $\tilde{V}_{444}(2x, 2y)$ prior to color conversion.

## Filter Equations

The filter is defined by two equations that compute new values $\tilde{U}_{444}(2x, 2y)$ and $\tilde{V}_{444}(2x, 2y)$ based on a weighted average of four neighboring values.

### Equation for $\tilde{U}_{444}(2x, 2y)$

$$
\tilde{U}_{444}(2x, 2y) = \frac{U_{444}(2x, 2y) + U_{444}(2x + 1, 2y) + U_{444}(2x, 2y + 1) + U_{444}(2x + 1, 2y + 1)}{4}
$$

### Equation for $\tilde{V}_{444}(2x, 2y)$

$$
\tilde{V}_{444}(2x, 2y) = \frac{V_{444}(2x, 2y) + V_{444}(2x + 1, 2y) + V_{444}(2x, 2y + 1) + V_{444}(2x + 1, 2y + 1)}{4}
$$

## Structure Summary

- **Input**: The filter operates on the original values $U_{444}$ and $V_{444}$ at positions $(2x, 2y)$, $(2x+1, 2y)$, $(2x, 2y+1)$, and $(2x+1, 2y+1)$.
- **Output**: The filtered values $\tilde{U}_{444}(2x, 2y)$ and $\tilde{V}_{444}(2x, 2y)$ are computed as the average of the four input values.
- **Purpose**: This is a spatial averaging filter applied prior to color conversion, likely to smooth or reconstruct chrominance components in a 4:4:4 format.
