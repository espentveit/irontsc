# Main view:

- **Area B1**: $Y_{420}(x,y) = Y_{444}(x,y)$, where the range of $(x,y)$ is $[0, W-1] \times [0, H-1]$

- **Area B2**: $U_{420}(x,y) = \tilde{U}_{444}(2x, 2y)$, where the range of $(x,y)$ is $[0, \frac{W}{2} - 1] \times [0, \frac{H}{2} - 1]$

- **Area B3**: $V_{420}(x,y) = \tilde{V}_{444}(2x, 2y)$, where the range of $(x,y)$ is $[0, \frac{W}{2} - 1] \times [0, \frac{H}{2} - 1]$

# Auxiliary view:

- **Area B4**: $Y_{420}(x,y) = U_{444}(x, 2y + 1)$, where the range of $(x,y)$ is $[0,15] \times [0,7]$

- **Area B5**: $Y_{420}(x, 8 + y) = V_{444}(x, 2y + 1)$, where the range of $(x,y)$ is $[0,15] \times [0,7]$

- **Area B6**: $U_{420}(x,y) = U_{444}(2x + 1, 2y)$, where the range of $(x,y)$ is $[0,7] \times [0,7]$

- **Area B7**: $V_{420}(x,y) = V_{444}(2x + 1, 2y)$, where the range of $(x,y)$ is $[0,7] \times [0,7]$
