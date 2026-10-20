Small checked Oberon frontend library for the separate Rust native compiler. Uses `syntax` and `sema` only; no code-generation crate dependencies.

Supported procedure subset: value `INTEGER` parameters, scalar `INTEGER` locals, fixed one-dimensional local `ARRAY n OF INTEGER` declarations with literal nonzero lengths up to 1024, assignments, indexed array expressions and stores, IF/ELSIF, WHILE and integer-returning procedures. All unsupported constructs fail closed.

The projection exports arrays through `Function.arrays: Vec<ArrayLocal>`, with `ArrayLocal { name, length }`, `Expr::ArrayElement` and `Stmt::AssignArray`. Array indices are semantic-checked expressions. The consuming compiler owns frame layout, zero initialization, signed bounds traps, SSA and machine code. This bridge does not expose array ABI parameters, open/multidimensional arrays, pointer/record types, aliases or calls.

The native compiler pins this repository to a reviewed commit; changes to the bridge require updating that pinned Git revision and running focused driver tests.