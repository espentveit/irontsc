```markdown
# Main view:

- **Arrear B1**: $Y_{420}(x,y) = Y_{444}(x,y)$, where the range of $(x,y)$ is $[0, W-1] \times [0, H-1]$.
- **Arrear B2**: $Y_{420}(x,y) = \tilde{U}_{444}(2x, 2y)$, where the range of $(x,y)$ is $[0, \frac{W}{2} - 1] \times [0, \frac{H}{2} - 1]$.
- **Arrear B3**: $V_{420}(x,y) = \tilde{V}_{444}(2x, 2y)$, where the range of $(x,y)$ is $[0, \frac{W}{2} - 1] \times [0, \frac{H}{2} - 1]$.

# Auxiliary view:

- **Arrear B4**: $Y_{420}(x,y) = U_{444}(2x + 1, y)$, where the range of $(x,y)$ is $[0, \frac{W}{2} - 1] \times [0, H-1]$.
- **Arrear B5**: $Y_{420}\left(\frac{W}{2} + x, y\right) = V_{444}(2x + 1, y)$, where the range of $(x,y)$ is $[0, \frac{W}{2} - 1] \times [0, H-1]$.
- **Arrear B6**: $U_{420}(x,y) = U_{444}(4x, 2y + 1)$, where the range of $(x,y)$ is $[0, \frac{W}{4} - 1] \times [0, \frac{H}{2} - 1]$.
- **Arrear B7**: $U_{420}\left(\frac{W}{4} + x, y\right) = V_{444}(4x, 2y + 1)$, where the range of $(x,y)$ is $[0, \frac{W}{4} - 1
