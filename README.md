# Oberon-07 → C: a compiler and its design

An Oberon-07 compiler, written in Rust, that emits readable C11: one translation
unit per Oberon module, compiled by the host `cc`. The language is the report —
*The Programming Language Oberon*, N. Wirth, revision 1.10.2013 / 3.5.2016 —
plus the small, explicitly-listed extension set the plan keeps.

`books/` holds the reference material the design cites (the report and the
compiler textbooks). It is not part of the repository: see `.gitignore`.

## Layout

| path | what |
|---|---|
| `oberon/DESIGN.md` | the design: the grammar decisions with the conflicts that forced them, the check catalogue, the codegen rules, the risks (R1…R25) and the decisions taken where the report is silent |
| `oberon/PLAN.md` | the backlog, item by item, with the evidence each item rests on |
| `oberon/src/` | the compiler: lexer, LALRPOP grammar, AST, resolver/checker, C emitter, and the two CLIs (`oberon-front`, `obc`) |
| `oberon/golden/` | the report's own procedures hand-lowered to C, a two-module link model, and the constant table the folder is diffed against — compiled against the runtime the compiler emits, run plain and under ASan/UBSan |
| `oberon/csmoke/` | every C emission rule with a non-obvious semantic, compiled and executed |
| `oberon/e2e/` | the whole compiler end to end: Oberon modules → C → `cc -Werror` → run → compare |
| `oberon/fuzz/` | random Oberon programs that know what they must compute, used as a self-checking oracle (`src/bin/obgen.rs`) |

## Build

```
cargo build --manifest-path oberon/Cargo.toml      # the compiler (needs a network-free lalrpop)
cargo test  --manifest-path oberon/Cargo.toml      # 68 unit tests
```

## Gate

Every change keeps these green (PLAN.md's preamble is the same list):

```
cargo test --manifest-path oberon/Cargo.toml
bash oberon/golden/run.sh      # 67 cases, plain and under ASan/UBSan
bash oberon/csmoke/run.sh      # 67 C-level cases
bash oberon/e2e/run.sh         # 55 checks, end to end through the driver
bash oberon/fuzz/run.sh        # the seeds that once found a defect, then 100 more
cargo clippy --all-targets --manifest-path oberon/Cargo.toml -- -D warnings
```

That list is what CI runs (`.github/workflows/gate.yml`), on a clean checkout.

`bash oberon/fuzz/run.sh` runs `oberon/fuzz/regressions.txt` — every seed that once
found a defect, each with the reason it is listed — and then a coverage block;
`SEEDS`/`BASE` select one contiguous range instead. A seed reproduces a random
program exactly, together with the values it must compute, so the list is a
regression suite rather than a memory of one.

## Using it

```
obc check Foo.mod -I DIR        # diagnostics only: located errors, warnings, an exit code
obc emit  Foo.mod --out DIR     # Foo.c, Foo.h and the runtime header
obc build Foo.mod -o Foo        # resolve the import graph, emit, and call cc
obc run   Foo.mod --command Foo.Run
```

The emitted C is written to compile with

```
-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror
```

`-fwrapv` is load-bearing: Oberon's `INTEGER` wraps, and the design fixes that as
the specified behaviour (DESIGN §9.1, R4) rather than leaving it to C.
