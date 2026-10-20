# Standalone compiler subset generators

The pinned Oberon `obgen` supports two independent, deterministic
compiler-subset families. Neither replaces the existing full Oberon
generator or its regression corpus.

## Structured arithmetic and CFG mode

Use `--base N --compiler-subset=source`,
`--compiler-subset=selfcheck`, or `--compiler-subset=harness`.
The seeded abstract syntax tree and independent interpreter produce
44 expected signed INTEGER results per seed. The three renderers use
the identical exported `Fuzz.Probe(a,b,c)` procedure.

## Fixed-array mode

Use `--base N --compiler-subset=array-source`,
`--compiler-subset=array-selfcheck`, or
`--compiler-subset=array-harness`. This mode exercises a checked
one-dimensional local `ARRAY n OF INTEGER` and scalar locals,
with deterministic lengths chosen among 2, 3, 5, 8, 17, and 200.

Each program executes bounded indexed loops, writes and reads array
elements, branches on signed comparisons, updates scalar induction
variables, and touches both array endpoints. Its separate Rust
interpreter uses explicit wrapping 32-bit arithmetic to calculate
44 expected results including signed extrema. Source and selfcheck
variants have the **same procedure body**; only the selfcheck module
adds reference-backend `ASSERT` calls. The C harness compares the
standalone compiler's native results to those independently calculated
values.

Example:

```sh
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 4 --compiler-subset=array-source
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 4 --compiler-subset=array-harness
```

Invalid array indices are excluded from the cross-backend *value*
oracle because runtime trap conventions differ. The standalone native
compiler's UD2/SIGILL bounds behavior is tested separately.

The consuming compiler repository pins the Oberon revision and runs
small cached array differential smoke checks; its full scalar/array
fuzz campaign remains manual-only. See
`bingundu/compiler/tests/oberon_fuzz.sh`.
