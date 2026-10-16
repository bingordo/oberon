# Oberon-07 -> C: a compiler and its design

An Oberon-07 compiler, written in Rust, that emits readable C11: one translation
unit per Oberon module, compiled by the host `cc`. The language follows the report,
*The Programming Language Oberon*, N. Wirth, revision 1.10.2013 / 3.5.2016,
plus the small, explicitly-listed extension set the plan keeps.

`books/` holds the reference material the design cites (the report and the
compiler textbooks). It is not part of the repository: see `.gitignore`.

## Layout

| path | what |
|---|---|
| `oberon/DESIGN.md` | the detailed grammar, check catalogue, code generation rules, risks and design decisions |
| `oberon/REFERENCE.md` | the `obc`, `oberon-front` and `obgen` command-line options and the supported Oberon language |
| `oberon/PLAN.md` | the backlog, item by item, with the evidence each item rests on |
| `oberon/crates/` | the compiler, split by phase (DESIGN section 2): `syntax` (lexer, LALRPOP grammar, AST, diagnostics), `sema` (resolver, checker, constant folder), `cgen` (the C back end), `clif` (the Cranelift back end), and `obc` (the facade plus the `oberon-front`, `obc` and `obgen` binaries) |
| `oberon/golden/` | the report's own procedures hand-lowered to C, a two-module link model, and the constant table the folder is diffed against, all compiled against the runtime the compiler emits and run plain and under ASan/UBSan |
| `oberon/csmoke/` | every C emission rule with a non-obvious semantic, compiled and executed |
| `oberon/e2e/` | the whole compiler end to end: oberon modules -> C -> `cc -Werror` -> run -> compare |
| `oberon/lib/` | modules a build puts on its search path rather than ones the compiler knows: `Out` in the shape programs written elsewhere expect, with `Args` (the host's arguments) written by the driver as an interface over the runtime |
| `oberon/fuzz/` | random Oberon programs that know what they must compute, used as a self-checking oracle (`crates/obc/src/bin/obgen.rs`) |

## Build

```
cargo build --manifest-path oberon/Cargo.toml      # the compiler (needs a network-free lalrpop)
cargo test  --manifest-path oberon/Cargo.toml      # 82 unit tests
```

## Gate

Every change keeps these green (PLAN.md's preamble is the same list):

```
cargo test --manifest-path oberon/Cargo.toml
bash oberon/golden/run.sh      # 67 cases, plain and under ASan/UBSan
bash oberon/csmoke/run.sh      # 88 C-level checks
bash oberon/e2e/run.sh         # 152 end-to-end checks through the driver
bash oberon/fuzz/run.sh        # 86 generated programs in seven blocks, on either back end
cargo clippy --all-targets --manifest-path oberon/Cargo.toml -- -D warnings
```

That list is what CI runs (`.github/workflows/gate.yml`), on a clean checkout.

`bash oberon/fuzz/run.sh` runs `oberon/fuzz/regressions.txt`, every seed that once
found a defect, with its reason for inclusion, as a value oracle and a trap
oracle, against the collector with a collection forced on every allocation, and in
the `-Odialect=lower` dialect; then a coverage block and an adversarial one, where
each program ends in a deliberate violation and the run must produce the exact trap
line, source byte included. `SEEDS`/`BASE`/`COVER`/`ADVCOVER`/`SAN` select other runs,
and `BACKEND=clif` runs the corpus, and the `e2e` suite, against the Cranelift back end
instead of the C one; `OB_GC_LIMIT`/`OB_GC_STRESS` exercise the collector with a
bounded heap or on every allocation. A seed reproduces a random program exactly, together with the values it must
compute, so the list is a regression suite rather than a memory of one.

## Using it

```
obc check Foo.mod -I DIR        # check the module and imports; print located diagnostics
obc emit  Foo.mod --out DIR     # module C/header/interface files and runtime support
obc emit  Foo.mod --out DIR --line-directives # map C diagnostics back to Foo.mod
obc build Foo.mod -o Foo -Ochecks=nil   # runtime checks: all (default), none, or a list
obc run   Foo.mod --command Foo.Run
obc build Foo.mod -o Foo --backend=clif  # emit an ELF object with Cranelift, then link
```

See the [command-line and language reference](oberon/REFERENCE.md) for every option,
output mode, supported construct, and deliberate language exclusion.

The C back end uses these required base flags:

```
-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror -Wno-overflow
```

`-fwrapv` ensures Oberon's `INTEGER` wraps, as specified in the design (section 9.1, R4),
instead of leaving it to C.
