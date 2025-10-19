# Mathematical Expressions

The image displays two mathematical expressions, each defining a function in terms of absolute differences and conditional operations.

## Expression 1: U₄₄₄(2x, 2y)

The first expression defines the function $ U_{444}(2x, 2y) $:

$$
U_{444}(2x, 2y) = \left( \text{abs} \left( \tilde{U}_{444}(2x, 2y) - U_{444}(2x, 2y) \right) > 30 \right) ? U_{444}(2x, 2y) : \tilde{U}_{444}(2x, 2y)
$$

This is a ternary conditional operator. It evaluates the absolute difference between $ \tilde{U}_{444}(2x, 2y) $ and $ U_{444}(2x, 2y) $. If this difference is greater than 30, the function returns $ U_{444}(2x, 2y) $; otherwise, it returns $ \tilde{U}_{444}(2x, 2y) $.

## Expression 2: V₄₄₄(2x, 2y)

The second expression defines the function $ V_{444}(2x, 2y) $:

$$
V_{444}(2x, 2y) = \left( \text{abs} \left( \tilde{V}_{444}(2x, 2y) - V_{444}(2x, 2y) \right) > 30 \right) ? V_{444}(2x, 2y) : \tilde{V}_{444}(2x, 2y)
$$

This expression follows the same structure as the first. It evaluates the absolute difference between $ \tilde{V}_{444}(2x, 2y) $ and $ V_{444}(2x, 2y) $. If this difference is greater than 30, the function returns $ V_{444}(2x, 2y) $; otherwise, it returns $ \tilde{V}_{444}(2x
