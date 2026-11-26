# Standalone compiler subset generators

The pinned Oberon `obgen` supports several independent, deterministic
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

## Type-directed recursive and imported-module modes

The `typed-source`, `typed-selfcheck`, and `typed-harness` modes
render the same bounded, recursively generated INTEGER/BOOLEAN AST and a
separately compiled C differential oracle (no interpreter). The generated
program always exercises wrapping arithmetic, bounded array reads and writes,
short-circuit side effects, scalar and indexed `VAR INTEGER`, `VAR BOOLEAN`,
and aliased and distinct `VAR` formals. The common type-directed `Place` AST
handles both scalar and bounded indexed INTEGER lvalues, including recursively
indexed array elements. Each generated seed covers same-address array aliasing,
distinct array elements, and mixed scalar/array VAR pairs, in addition to
scalar aliases. The generator now also tests distinct index ASTs that
are guaranteed to select the same element (such as `a` versus `a + 4`
modulo a four-element array), including nested reads and MIN/MAX 32-bit
wraparound. The imported/local `Twist` helper now performs ordered
read-after-write and write-after-write operations, making aliased and distinct
VAR arguments observably different. Each generated seed includes a three-call
sequence that mutates the source of an array subscript, passes equivalent
addresses computed from its updated value, and then recomputes the subscript
after the call. The differential result includes **all four final array
elements**, not just the two endpoints, so incorrect reloads and writes to
interior elements are visible. Generated indices are pure to avoid unspecified
argument side-effect evaluation order in the independent C oracle.

The `typed-import-*` modes use the **same seed and AST** but move the
by-reference helpers into a separately compiled Oberon module:

```sh
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-import-provider > FuzzOps.mod
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-import-source > Fuzz.mod
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-import-harness > reference.c
```

`Fuzz` imports `Ops := FuzzOps`, calling exported `Change`, `Twist`, and
`FlipFlag` through checked procedure interfaces. `Twist(x,x,delta)`
explicitly checks same-address aliasing; indexed `Change(cells[...],...)`
checks address lowering and bounds. The source mode is suitable for compiling
and linking two modules independently; the harness compares both exported
`Fuzz.Probe` and `Fuzz.Mirror` with a C reference on 32 fixed/seeded inputs.
Use `typed-import-selfcheck` instead of `typed-import-source` to emit the
additional module-level metamorphic checks. The provider contents are
seed-independent. Native end-to-end execution belongs on
`bingordo/compiler-ci`, not the source repository's Actions.

## Three-module VAR forwarding differential

The `typed-relay-*` modes add a third native module between the generated
consumer and the operations provider, without changing the typed AST or the
independent C oracle. `Fuzz.Probe` calls `FuzzRelay.Forward` with scalar
and indexed `VAR INTEGER` arguments; the relay forwards **the original
addresses** to `FuzzOps.Twist` and `FuzzOps.Change`, then reads the
mutated values again. This stresses pointer/VAR forwarding through two
call boundaries, distinct expressions selecting one array address,
same-scalar aliases, and mixed scalar/array actuals. The same type-directed
AST also generates relayed `VAR BOOLEAN` calls (`ForwardFlag`) and
heterogeneous `VAR INTEGER` + `VAR BOOLEAN` calls (`ForwardMixed`), with
both Boolean guard values, short-circuit `Touch` effects, and read-after-write
mutations in the callee. Each seed includes mandatory witnesses and recursive
randomized calls for both new signatures. The C oracle models the helper
semantics independently with defined 32-bit wrapping.

```sh
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-import-provider > FuzzOps.mod
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-relay-provider > FuzzRelay.mod
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-relay-source > Fuzz.mod
cargo run --manifest-path oberon/Cargo.toml --bin obgen -- \
  --base 42 --compiler-subset=typed-relay-harness > reference.c
```

The prior `typed-import-*` two-module modes remain supported; their
`Forward` helper is exported from `FuzzOps` instead of forwarding
through `FuzzRelay`. Both paths use identical source-level helper
semantics and the same C reference. Source generation is covered by
128-seed unit tests; full GAS/direct-ELF differential execution is
performed in `bingordo/compiler-ci`.
