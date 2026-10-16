# Oberon-07 -> C: compiler backlog

This is the open work, derived from `DESIGN.md` rather than from enthusiasm.
Every item names its source section or risk and the files it touches. It also
states what makes the work done. Paths are relative to this directory.

An item is done when **the four suites and clippy pass** and the evidence it
promises exists as a test or a suite check:

```
cargo test                  # 71 unit tests
bash golden/run.sh          # 67 golden cases, run twice: plain and under ASan/UBSan
bash csmoke/run.sh          # 67 C-level cases
bash e2e/run.sh             # 55 checks, end to end through the driver
bash fuzz/run.sh            # the seeds that once found a defect, then 100 for coverage
cargo clippy --all-targets -- -D warnings    # green (W0.1)
```

`bash fuzz/run.sh` joins it (W6.2): by default it runs `fuzz/regressions.txt`,
the seeds that once found a defect, each with its reason for inclusion. Each
seed reproduces a program exactly on any machine. The script then runs a
100-seed coverage block. `SEEDS`/`BASE` select one contiguous range for an ad hoc run.
`COVER` sets its size. The first run found five failing seeds. The fixes are in place.
The regression list keeps those cases.

Recently closed: DIAG's located diagnostics and statement-level recovery
(DESIGN sections 5 D7 and 10). The CLI warning path used to hide warnings in a clean
module. This document moved into the crate. W6.1 added the random program generator.

---

## Where we are

| milestone | state |
|---|---|
| M0 lexer + grammar + AST | done |
| M1 resolver | done |
| M2 checker + folder | done except the four gaps in W1 |
| M3 C emitter (types, declarations, expressions, control) | done |
| M4 procedures, parameter modes, open arrays, `NEW`, guards, type `CASE` | done |
| M5 modules, imports, commands, interface files | done |
| M6 diagnostics, `-Ochecks`, golden tests | diagnostics done, golden done, **`-Ochecks` not implemented** (W3.1) |
| M7 driver | done |

---

## W0: make the document's own claims true

**W0.1 clippy is not clean** (done)
Why: the preamble says "clippy clean"; `cargo clippy --all-targets` reports 244
warnings, all in LALRPOP's generated parser
(`target/.../out/oberon.rs`, `clippy::ptr_arg`: LALRPOP's `ErrorRecovery`
signature requires `&mut Vec`, so it cannot be changed).
Files: `crates/syntax/src/lib.rs` (the `lalrpop_mod!` site).
Done when: the generated module has an `#[allow(clippy::ptr_arg)]`, scoped to it
rather than the crate. Then `cargo clippy --all-targets -- -D warnings` exits 0,
and the clippy line is part of the gate above. **Done**: `lalrpop_mod!` carries
the attribute (LALRPOP's recovery hook uses `&mut Vec<..>` by its API, so
`ptr_arg` fires 244 times inside the generated file and nowhere in ours). The
gate block below no longer has a caveat.

**W0.2 sections 9.2 and 9.3 were marked `[design]` although the suites exercise them** - **done**
Why: section 9.2 covers type mapping. Section 9.3 covers records, extension and descriptors.
`crates/cgen/src/cgen.rs` has both. The `e2e` and `csmoke` suites exercise them.
Done when: those markers say what is actually unverified (if anything), so that
`grep '\\[design' DESIGN.md` returns only the legend. **Done**: sections 9.2
and 9.3 are now `[verified]`, with the suites that execute them named. The
document's legend defines `[design]` as "not yet implemented", and every row of
the mapping table and every descriptor rule is exercised by `csmoke`, `e2e` or
`golden`. `grep '\\[design' DESIGN.md` now returns the legend and nothing else.

---

## W1: rules the checker still does not enforce

These are DESIGN section 8.5's own list of four unchecked things, quoted verbatim.
**W1.1 field visibility across modules** (rows 4 and 14's private-field halves,
report section 6.3) - **done**
Why: a field's `*` is accepted, but this pass lacks the data to enforce
interface-layer access restrictions.
1. `types::Type::Record { base, fields: Vec<(String, TypeId)> }` has no
   per-field export mark;
2. `cgen::render_type` renders a record as `RECORD (Base)` without fields, so an
   interface cannot carry them. The hand-written `Texts.sym` fixture (per
   `e2e/run.sh`'s header) carries a field list the emitter would never produce;
3. `sema` cannot compare the accessing module with the defining one.
Files: `crates/sema/src/types.rs`, `crates/cgen/src/cgen.rs`, `crates/sema/src/sym.rs`, `crates/sema/src/sema.rs`.
Done when: the interface records the mark and private-field access on an imported
record is an error. `check_tests` has a case for each row, named by row number.
**Done**: `types::Field` records the mark. The writer already
emitted field lists and the reader already parsed them, so the work was the mark,
not the format. `interface_env` marks every type an interface declares, so the
field selector can tell the record came from another module. The writer emits
each field's own mark instead of a `*` on all of them. The fixtures needed
correction too: the report's own `Out.WriteLn` reads `W.buf` from `Texts`, and
`buf` is exported in the report's library although the hand-written `Texts.sym`
said otherwise. Every row of section 8.5 is now checked.

**W1.2 row 50's locality half** (report section 6.5, section 10.1) - **done**
Why: a *local* procedure assigned to a procedure variable is rejected, but one
passed as an *actual* is not. The check exists on one side of a symmetric rule.
Files: `crates/sema/src/sema.rs`.
Done when: passing a nested procedure where a procedure variable is expected is
an error, with a test. **Done**: `check_actuals` rejects a `local procedure`
actual when the formal's type is a procedure type (the formal is checked first so
a plain type mismatch is not reported twice). This makes the rule hold for both
halves of the report's rule: `f := Inner` and `Taker(Inner)`. Catalogue row 50
now has both shapes, and a probe confirms the module-level case still passes.
Section 8.5's list of unchecked rules is down to two (both halves of field
visibility, W1.1).
**W1.3 warn on unused local procedures** (done)

Why: unused imports warn, but an unused local procedure does not.
Files: `crates/sema/src/sema.rs` (the `unused_imports` pass is the natural home).
Done when: an uncalled local procedure warns once with a location through
`obc check`, the path fixed this session.
**Done**: `Sema::unused_procedures` records uses where every *use* of a name
passes through (`lookup_qual`). Silent probes call `lookup` instead and do not
count. Identities are assigned per declaration so shadowing cannot confuse it,
and the caret marks the procedure's *name*. This needed a span in the AST
(`Proc::name_span`, captured by the grammar) rather than arithmetic on the
declaration's span. A call, a use as a value and an exported procedure all count
as used. A self-recursive but otherwise unreachable procedure counts as used,
which the code notes as the imprecision. Tested by
`diag_tests::an_uncalled_procedure_warns_and_a_used_one_does_not` (module-level
and nested, plus four negative cases). DESIGN section 8.5's list of unchecked
rules is down to three.

**W1.4 settle section 13's open questions and delete them** - **done**
Why: three questions have been open since the design was written. The code already
answers one.
- `ARRAY 0 OF T` is already an error: `an array length must be positive` (report section 6.2). Pin it with a test and remove the question.
- A field that hides a base record's field was proposed to be allowed with a warning. Decide and record the rule.
- The design proposes warnings for unused imports to be off by default. The compiler warns by default. Keep that behavior because the suite's clean modules do not import unused modules, then make the design and code agree.

**Done**: all three are now decisions in DESIGN section 13 and the stale "Open
questions" block is gone (it was addressed "when M1 starts", and M1 is finished).
`ARRAY 0 OF T` was already an error and is pinned by catalogue row 41; the
field-hiding rule is now implemented rather than only allowed. The checker warns
per field name, with the diagnostic naming why (`diag_tests::a_field_that_hides_an_inherited_field_warns`). Unused imports keep warning by
default, which section 13 records.

---

## W2: known deviations with a real failure mode

**W2.1 R17: the reclassification pass uses module-wide name sets** - **done**
Why: `resolve.rs` splits guard-vs-call and folds module qualification *before*
the checker builds scopes, so a local variable that shadows an import alias, or
a nested type name, is classified by the wrong scope. This is a correctness
bug for programs that shadow, not a style matter.
Files: `crates/sema/src/resolve.rs`, `crates/sema/src/sema.rs` (it already builds the scopes the pass
needs).
Done when: a module whose local shadows an import alias checks correctly (or is
rejected for the right reason), with a test. Then R17 in section 13 becomes a note
about the prototype's pass order, not a live limitation. **Done**:
the pass keeps one scope per procedure, including parameters and declarations.
It records every declaration, not just types and import aliases. A variable can
shadow a type with the same name. That was the bug: a local `T` was invisible,
so `Q(T)` stayed classified as a guard.
Both formerly rejected programs now pass the check. Section 13's R17 records the
before-and-after behavior, and a test covers both cases.

**W2.2 R15: a failing guard with the record spelling is statically out of bounds** - **done**, and the cause was elsewhere
Why: `p(S).b` reads through a pointer to `R`, which cannot hold an `S`.
Gcc's `-Warray-bounds` and `-Wmaybe-uninitialized` warnings reject the program
under `-Werror` even though the guard's tag test traps first. The e2e case uses
the pointer spelling, which has no such read.
Candidate fixes: one option lowers the guarded access through a union of possible
record layouts. Another uses an opaque `char *` plus `memcpy`. Both keep the trap
first. A local pragma remains a documented fallback.
Done when: the program compiles under both spellings with gcc and clang
and still traps with the right offset. **Done**: the obstacle was `rt_new` being
`static inline` in the header, which let gcc fold `malloc(sizeof(R))` and prove the
guarded write was outside the allocation; it is defined in `oberon_runtime.c` now,
and the failing record-spelling write compiles and traps with the guard's offset
under both compilers (`trap-guard-record-spelling-as-target` for gcc, and the same
program built with `--cc clang`). The pointer spelling was already tested.

**W2.6 a `NIL` pointer where a type is asked for faults on the C path** - **done**

**Found by W9.2b:** the same module behaved differently on the two back ends.
Both `p IS T` and a type-`CASE` on a NIL pointer read a descriptor at address
zero and faulted. The guard checked first.

**Fixed** in the same change as the finding: `IS` orders its test as the guard does,
and a pointer type-`CASE` switches on the "no type" sentinel `0`, which no label
carries, leaving the default arm or W2.5's located trap to answer. Row:
`nil-has-no-type`, which fails on the old code by dying.

**W2.5 a scalar `CASE` without a matching label doesn't produce a result** - **done**

**Found while writing the second back end (W9.1).** It initially matched the C
path. The policy decision below changed both back ends.

Report section 9.5 defines which arm runs and requires matching label types. It says
nothing about a value that matches no label. Both behaviors are allowed, but this
compiler is inconsistent: a type-`CASE` with no match traps with its source offset
and warns when it has no default arm (catalogue row 39 and section 9.4). A scalar
`CASE` with no match and no default has no diagnostic, leaving its result undefined.
A program that relies on that behavior would change. This is a policy decision.

Options and their costs: **trap** matches the type-`CASE` rule and the design's
"trap rather than silence" policy. It changes emitted C for scalar `CASE` without
a default. **Warn and keep the current behavior** preserves execution but makes
catalogue row 39's warning apply. **Leave both behaviors** and document why the
asymmetry is deliberate.

**Decision: warn and trap, matching type-`CASE`.** The design prefers a located
diagnostic over a silent undefined result. A type-`CASE` without a default warns;
a value that matches no label traps with its source byte. Scalar `CASE` follows
the same rules.
This changes generated C for scalar `CASE` without a default: it gets a trapping
default arm and a warning. Section 9.6 records this as a deliberate choice.

Done when: section 9.6 names this choice, the compiler warns only when a default
arm is absent, and the trap includes the source byte. The C and Cranelift back ends
must agree. W9.2b lowers this shape, and both suites pin the exact trap line and
the warning. **Done**: the emitter writes a trapping default under `-Ochecks=case`.
The checker warns for every `CASE` without a default arm (catalogue row 39).
Section 9.6 records the rule. Cranelift uses the same message, byte (the subject's
span), and switch. Rows:
`trap-case-no-matching-label` (asserting the exact line and abort),
`a-default-less-case-warns`, and fixtures for the prior behavior changed to use
default arms. These include `Regress.mod`'s ranged `CASE`s, the kitchen-sink
module, and the early-`RETURN` row. No-match cases no longer return silently.

**W2.4 a pointer array cannot see a record declared later in the module** - **done**

**Found while building the collector** (W10.2), and not caused by it: the same
output comes from the compiler at HEAD. A legal Oberon-07 program does not build:

```
MODULE T3;
  TYPE P = POINTER TO R; R = RECORD x: INTEGER END;
  VAR a: ARRAY 2 OF P;
BEGIN NEW(a[0]); a[0].x := 1
END T3.
```
```
./T3.h:6:21: error: unknown type name `T3__Anon_1`
./T3.c:16:51: error: request for member `desc` in something not a structure or union
```
The emitter has two causes: ordering and naming.

1. **ordering.** `type_rank` gives an array `100 + rank(elem)` and a record `200 + ...`,
   and a pointer ranks 0, so `ARRAY 2 OF P` is emitted *before* the record it points
   at. A pointer field does not impose an order (as the comment in `type_rank`
   says), and neither does a pointer in an array. Such a use needs a *forward*
   declaration, which is missing.
2. **naming.** naming a composite happens on first *visit*, and the visit reaches
   `R` through `P`'s pointer arm while `R` is not yet declared, so it is named
   `T3__Anon_1` instead of `T3__R`, as the interface and later uses call it.
   A pre-pass over the module's declared types would know the name before any
   visit. It preserves the deliberate "name on visit so recursive types
   end" rule.

A sibling of the same root cause, and the worse one: a *cross-module* pair
(`TYPE P* = POINTER TO Node; Node* = RECORD ... END`) names it `Def__Anon_1` in the
header but `Def__Node` in the interface, so the header and the `.sym` disagree.

Done when: a pointer array of a same-module record compiles and runs, the
cross-module pair's header and interface agree on the name, and both are pinned.
`e2e`'s pointer-array row currently borrows its pointee from another module, which
is exactly what this hides. **Done**, by the two changes the causes called for: the
module's *declared* type names are collected before any type is visited, so a
composite reached through a pointer keeps its own name instead of an anonymous one;
and every named composite gets a forward `typedef struct X X;` before all the
definitions, so `type_rank`'s ordering no longer matters for a pointer use (the
definition repeats the same typedef, which C11 allows and `-Wall -Wextra` accepts).
Rows: `pointer-array-of-a-later-record` (compiles and runs), the cross-module pair
(`pointer-to-a-record-in-another-module`), and `neither-side-names-the-pointee-anonymously`,
which asserts the bug's own signature: no `Anon` name survives on either side.

**W2.3 R16: a structured value argument inside a short-circuit operand is copied eagerly** - **done**
Why: a record argument inside `AND`/`OR` whose own index expressions contain
calls is copied one step earlier than Oberon evaluates it. This is a documented
deviation.
Done when: the copy happens where the operand is evaluated (or the design
`&&`/`||` operands hoist the copy's declaration. The copy expression is a comma
expression whose value is the copy's address: `((rt_rec_copy(&tmp, &(src), ...),
&tmp))`. This evaluates the copy and any call in the actual's selector where the
language evaluates the operand. `e2e`'s `copy-inside-shortcircuit` counts the
calls in both directions (none when short-circuited, one when not) and traps
under the old code.

---

## W3: toolchain, driver, packaging

**W3.1 `-Ochecks` / `-Ochecks=n`** (M6, section 9.4, R6) - **done**
Why: the design says runtime checks are on by default and compiled out with
`-Ochecks=n`; in fact `cgen` emits `rt_trap` unconditionally and the driver has
no such flag.
Files: `crates/cgen/src/cgen.rs` (an option threaded to the three check sites: index, `NIL`
dereference, type-`CASE` no-match), `crates/obc/src/bin/obc.rs`, `e2e/run.sh`.
Done when: a bounds violation traps by default and runs without the check under
`-Ochecks=n`, both covered by suite checks; R6's `FOR`-at-`MAX(INTEGER)` note is
resolved in the same pass (the trap is one of the checks the flag turns off).
**Done**: `-Ochecks=n` removes the index check, the `NIL` check, a guard's tag test
`e2e` has four rows that assert the module traps by default (with the offset) and
exits 0 without the checks. They also assert that the unchecked build's emitted C
has no `rt_index`/`rt_nil`/`rt_guard_is` at all. The divisor checks stay, because
they keep the arithmetic *defined* rather than unchecked. `ASSERT` is a statement
rather than a check. Both rules are stated in section 9.4. R6 is resolved as
defined-by-the-report rather than checked. See its row.

**W3.2 interface-file versioning** (section 7, "prototype codec") - **done**
Why: the format starts at `MODULE <name>` with no version, so a stale interface
from an older compiler is indistinguishable from a current one.
Files: `crates/sema/src/sym.rs` (both directions), `crates/sema/src/sema.rs` (the error).
Done when: the format writes a version line and checks it. A wrong-version
interface is refused with a located message. Round-trip tests cover it. **Done**:
`sym::FORMAT_VERSION` is written as the second line (so a reader knows what it is
reading before any declaration) and checked on every parse; a differing version or
a missing line is refused with a message that says to recompile, pinned by
`sym::tests::refuses_an_interface_from_a_different_version`. The hand-written e2e
interfaces and the fixtures carry the line; in the checker's own fixtures it is
supplied by the helper, since those fixtures are about the language.
The driver's diagnostic path had two more findings. A refusal was keyed by the
file's name, so a hand-renamed `Alias.sym` holding `MODULE M` was looked for under
`Alias`. The check failed to explain the mismatch. It now uses the module name in
the file's header, which remains readable when the rest of the file is not. A
module whose *C* was found also skipped the refusal report because the driver's
early path was not taken. The checker's diagnostic now reports the refusal; it
knows an interface is needed regardless of the backing file. The driver's early
path and the checker's diagnostic are exclusive, so neither can report the refusal
twice. The e2e covers both outcomes:
`an-unrelated-refused-interface-is-not-reported` and
`a-refused-interface-that-is-needed-is-explained`.

**W3.3 continuous integration** - **done**
Why: there is no `.github/` and no CI. The gate is currently a human habit.
Done when: a workflow runs the four suites on a clean checkout with a C
compiler that has ASan/UBSan. The golden suite runs plain and sanitised, and `CC`
overrides the default gcc. A Rust toolchain at least as new as the one
`Cargo.lock` pins is also required. **Done**: `.github/workflows/gate.yml` runs the
list on `ubuntu-latest`. A build step comes first because the suites drive
`target/debug/obc`. Then it runs unit tests, golden (plain and sanitised), csmoke,
e2e, fuzz (regression list, traps, sanitizers, coverage) and clippy at `-D warnings`.
Every command in it passes on this machine, which is the evidence available here.
The YAML is parsed. The one thing a clean runner has that this machine does not
is a *fresh* gcc, which is also why `CC` is respected throughout.

**W3.4 README at the repository root** - **done**
Why: the only entry point is DESIGN.md inside the crate; a reader arriving at
the repository sees no statement of what it is.
Done when: a short README gives the build line and gate commands, and points to
the two documents (`oberon/DESIGN.md`, `oberon/PLAN.md`). **Done**: the repository
root now has `README.md`. It includes the layout table, build and gate commands,
and the driver's four verbs. It also gives the emitted C's build line and why
`-fwrapv` is in it, and points to W6.2 for the fuzzer's state.

---

## W4: designed with an escape hatch, not built

**W4.1 `SYSTEM`** (R5, report section 11.1): **done**
Why: `crates/sema/src/sema.rs` used to reject `IMPORT SYSTEM` outright ("module SYSTEM is not
available in this compiler (report 11.1)"). Report section 11.1 defines five
portable entries and one proper procedure, all of them deliberately outside the
type rules:

| entry | signature (report section 11.1) | meaning |
|---|---|---|
| `ADR(v)` | any variable -> `INTEGER` | address of `v` |
| `SIZE(T)` | any type -> `INTEGER` | size of `T` in bytes |
| `BIT(a, n)` | `(INTEGER, INTEGER) -> BOOLEAN` | bit `n` of `mem[a]` |
| `GET(a, v)` | `a: INTEGER`, `v`: any basic type | `v := mem[a]` |
| `PUT(a, x)` | `a: INTEGER`, `x`: any basic type | `mem[a] := x` |
| `COPY(src, dst, n)` | all `INTEGER` | copy `n` consecutive words, `src` -> `dst` |

The report also lists system-specific extras (`VAL(T, n)`, `ADC`,
`SBC`, `UML`, `COND`, `LED`). We refuse those *by name* ("SYSTEM.ADC is not
implemented") rather than silently, and do not support them.

Step 1: resolution. `IMPORT SYSTEM;` resolves to a builtin interface; no
`SYSTEM.mod` is read, and one on the search path is shadowed (documented, since
a user supply would be a different language). The entries are not ordinary
procedures: `ADR`/`GET`/`PUT` take a *variable* (an address is taken).
`SIZE`/`VAL` take a *type*, which requires the structure of section 5 D2: one
production, dispatched by name in the checker, rather than five magic signatures.

Step 2: the rules to add to the section 8.5 catalogue. `GET`/`PUT`'s variable must be
of a basic type (a record or array is an error). `SIZE`'s argument must be a
type name rather than an expression. `ADR`'s argument must be a designator with
an address, such as a variable or a value parameter of basic type. `COPY`'s
count is in words. These operations are unsafe by design. They do not perform
bounds or type checks. `-Ochecks` does not add those checks. SYSTEM provides the
escape hatch.

Step 3: codegen, inlined (the `ob_sys_` prefix stays reserved for the runtime):
`ADR(v)` -> `(intptr_t)(uintptr_t)&v`, rather than the `(int32_t)` first written here:
that truncates a stack address and the word access sign-extends the truncation, so
the round trip faults on a 64-bit host (measured; R31, DESIGN section 9.10); `SIZE(T)` ->
`(int32_t)sizeof(<C type of T>)`, the layout the record descriptors are built
beside, which the C compiler folds; `BIT(a, n)` ->
`((*(const uint32_t *)(uintptr_t)a >> ((uint32_t)n & 31u)) & 1u) != 0`;
`GET`/`PUT` -> a word load/store through `(uint32_t *)(uintptr_t)a`, narrowed to
the *typed* operand's width (`GET`'s variable, `PUT`'s value, as the report names no
other type). `COPY` -> `memcpy(dst, src, (size_t)n * 4u)`. `VAL` is not lowered:
with the other RISC extras it is refused by name, as step 2 says.

Step 4: decisions to record (the report is silent, so DESIGN section 13 grows):
* the address model. Report section 11.1's `mem[a]` is a *word* array; we define an
  address as a **byte** address, so `ADR(v)` is `&v` and `COPY`'s `n` counts
  words -> `n * 4` bytes. One sentence in section 9.4, and the deviation is visible.
* `SIZE(T)` is the byte size including padding, consistent with the descriptors.
* a module that imports SYSTEM is not portable, and its interface file records
  the import, so a client can see it.

Step 5: tests, as built. `e2e/Sys.mod` uses every entry and asserts each result
against a hand-computed expectation: `SIZE` of the basic types, an array and a
*stride* that ties `SIZE(R)` to the padded extent; an `ADR`->`PUT`->`GET` round trip
on both module statics and locals; `BIT` on a word written that way, including the
masked count; and `COPY`'s word count, with the words around the moved ones checked
unchanged. `csmoke` executes the emitted snippets (address round trip, `BIT`,
`GET`/`PUT` truncation and mask width, the word count and the `SIZE` extent). The
e2e block also runs the located negatives: the RISC extras refused by name,
`GET(a, record)`, `PUT(a, array)`, `SIZE(expr)`, `ADR(1)`, `GET(a, constant)`,
an argument count, and `SYSTEM.x` without `IMPORT SYSTEM`. The e2e tests also
check shadowing a `SYSTEM.mod`/`SYSTEM.sym` pair on the search path and byte-identical
C under `-Ochecks=n`.

**Done**: the tests above pass, and every claim below was executed:

* `DESIGN` section 9.10 documents the lowering and address model (one sentence of it
  in section 9.4, the decision in section 13's list, the deviation as R31).
  Section 8.4 states that `SYSTEM` is not a predeclared identifier. Section 1's
  out-of-scope bullet is gone, with SYSTEM in the in-scope list. Section 8.5's
  row 53 is the by-name refusal and rows 55-60 carry the entries' rules. R5 is
  "implemented, and the RISC extras still refused by name".
* The gate: `e2e` (its `system-*` rows), `golden`, `csmoke` (its `system-*`
  cases), `cargo test` (the catalogue rows and the four new `check_tests`, the
  `SYSTEM.SIZE` reclassification case) and `cargo clippy -D warnings` all pass.
* **Not in this item**: the generator's SYSTEM fragment. `W6`'s own text asks for
  one, and the fuzzer's mirror lives in `crates/obc/src/bin/obgen.rs`. W4.1 covers
  the compiler's half. **The sized accessors** W8.7 proposes (`GET8`...`PUT32`) are
  deliberately not taken (section 9.10): the report's table has none, and the address
  keeps the host's width instead, so byte access doesn't need a separate entry.
* **One deviation from this item's own step 3**, found while writing its test and
  recorded as R31: the address is `intptr_t` in the expression, not the
  `(int32_t)` first prescribed, because the `INTEGER` spelling faults on a stack
  address on a 64-bit host.
**W4.2 garbage collection** (R3): *superseded in form by W10.1, which carries the
same study and states the variables the decision turns on; kept here as the
original entry point. The design study is W4.5 and
comes first*
Why: `NEW` maps to `malloc`; a tracing collector is what real Oberon systems
have and what long-running programs notice. The hatch is `rt_new`, which the
document claims can be replaced without touching generated code.
Prerequisite to settle first: root discovery. The frame layout must let the
collector find the pointers it needs. The layout is a design constraint.
Done when: the design states the root model, and a stress program runs under
`-Ocollect` without leaking or crashing.

**W4.3 the crate split of section 2** (`crates/syntax`, `crates/sema`, `crates/cgen`,
`crates/obc`): **done**
Why: section 2 prescribes a split by phase. The prototype was one crate with `src/*.rs`.
It is mechanical because the phase interfaces are three plain data types, and it
pays off when a phase grows. `syntax` is the only crate that may know LALRPOP.
Done when: these crates exist with the same tests and suites green. **The mapping
below shows the mechanical split** (the modules as they stood, 19k lines in
one crate):

* `crates/syntax`: `lexer.rs`, `ast.rs`, `oberon.lalrpop` and its `build.rs`. Section 2's
  rule that this is the only crate which may know LALRPOP is the point of it.
* `crates/sema`: `resolve.rs`, `sema.rs`, `types.rs`, `sym.rs`, `const_eval.rs`
  (depends on `syntax`).
* `crates/cgen`: `cgen.rs`, `layout.rs`, `clif.rs`, `oberon_h.inc`,
  `oberon_runtime.inc` (depends on both). Section 2's names for these files (`emit.rs`,
  `layout.rs`, `runtime.h`, `mangle.rs`) sketch the same shape; ours are the
  ones that exist, and the difference is in naming.
* `crates/obc`: `bin/obc.rs`, `bin/obgen.rs` and `bin/oberon-front.rs` (the
  generator depends on nothing, so it can sit here; `oberon-front` is
  the front-end CLI the e2e suite drives as `../target/debug/oberon-front`).

`oberon/Cargo.toml` becomes the workspace manifest with those as members, so the
binaries stay in `oberon/target/debug/` (no suite path changes), and the CI
workflow's `--manifest-path oberon/Cargo.toml` keeps resolving. The tests move
with their modules (they are unit tests in the same files), and the shell suites
do not know any of this happened.

**Done.** Modules now sit in crates under `crates/{syntax,sema,cgen,obc}`, with a virtual
workspace manifest at `oberon/Cargo.toml` (`members = ["crates/*"]`, resolver 2), so
the binaries are routed to `oberon/target/debug/` and no suite or CI path changed.
The crate moves forced two additional moves that the note above did not name:

* `Diagnostic`, `Severity`, `dedup_diagnostics` and the diagnostic renderer moved out
  of `sema.rs`/`lib.rs` into `crates/syntax/src/diag.rs`. A lexical or parse error is a
  diagnostic, so its type has to be visible to the parser. Putting it in `syntax` lets
  the checker import it without a cycle and keeps `sema` and `cgen` from naming LALRPOP.
* `rendered_type` (the `.sym` type reader) moved from `sema.rs` into `syntax`, beside
  the grammar it calls; `sema` reaches it as `syntax::rendered_type`.

The API (`emit_c_full`, `emit_object_full`, `layout_table_full`, `check_module*`)
and the four test modules are `crates/obc/src/lib.rs`; the two front-end tests that
read a path needed one fix. `const_eval.rs`'s folder-vs-C check read
`golden/constexpr.tsv` relative to its manifest, so it reads
`../../golden/constexpr.tsv` now that the crate sits one level down.

**Done**: the numbers are identical to the one-crate tree. `cargo test` **81 passed**.
`cargo clippy --all-targets -- -D warnings` is clean at the workspace.
`bash golden/run.sh` passes with ASan/UBSan clean. `bash csmoke/run.sh` reports
**0 failures**. `bash e2e/run.sh` is **126/0**, as is `BACKEND=clif bash e2e/run.sh`.
`bash fuzz/run.sh` and `BACKEND=clif bash fuzz/run.sh` exit 0. The workspace
manifest at `oberon/Cargo.toml` is used by `cargo build|test|clippy --manifest-path
oberon/Cargo.toml`; the commands include the workspace members, so the CI workflow
resolves unchanged.

**Its back ends are crates of their own too**, as section 2's split by phase requires:
`crates/clif` holds `clif.rs` and `layout.rs`, and `cgen` is the C back end and
nothing else. The one inward edge is `clif -> cgen` (symbol mangling and the `Checks`
type), which the two back ends must agree on because an object from one links against a
translation unit from the other. The suite numbers above are unchanged. A module
named after its own crate is a resolution trap
(`use x::{x, ...}` shadows the crate name for the rest of that module), so the moved
files are reached by path.

**W4.4 a `.sym`-only fast path**: **deferred, measured** (the condition it waits for has not fired)
Why: `obc build` has no up-to-date check (nothing in the driver looks at
timestamps), so every module of the import graph is re-checked on each run. A
timestamped interface would skip the front end for unchanged modules.
Only worth doing if build times become a complaint. **Measured**: `obc check` of
`e2e/Torture.mod` runs lexing, parsing, resolution and checking, the whole front
end on the largest module in the tree, in **0.04 s**. `obc build` of the same
program (front end, emit, and `cc` over four modules) takes **0.73-0.98 s**. The
front end is about 5% of a build, so a `.sym`-only fast path would save a few
hundredths of a second per module. The item stays open because build times have
not prompted the complaint it addresses. Reusing unchanged C objects or `ccache`
would affect the C end instead. Those are separate changes and neither is needed
at these sizes.

**W4.5 the collector's design: a study before any code**: research recorded in
DESIGN section 15. W10.2 contains the code.
Why: GC is the one item in W4 whose *interface* decides its cost. The compiler
lacks two things a collector needs, and they are expensive to change later.
Resolve the design first. Read the reference compiler to answer most questions.

Project Oberon 2013 has a mark-sweep collector in `Kernel.Mod`
(`Mark*`/`Scan*`). Its type descriptor gives the object's **size** and a
**`-1`-ended list of pointer-field offsets** (read at `descriptor + 16`),
which tells it where the object's pointers are.
* Its roots are a **vector of pointer slots passed in by the caller**
  (`Mark*(pref)` walks it four bytes at a time), with any value below `heapOrg`
  treated as `NIL`. The compiler/kernel must be able to *enumerate* roots.
* Marking uses **pointer reversal** (a down/up walk that turns the object graph
  into its own stack), so collection can run without an auxiliary stack.
* Sweeping rebuilds **segregated free lists** (32/64/128/256 bytes) and keeps an
  `allocated` counter.
* On our side, objects already carry a `desc` pointer (`(*q).desc = &TD_Fr__R`),
  but `ObDesc` is only `{ base, id, name }`. It has no size or pointer offsets.
  Module variables are plain static globals, so the global root set is free.
  Non-nested procedures use ordinary C locals, where roots are not enumerable today.

Questions for the study, with sources to read:
1. **Roots**: the crux. (a) Precise: emit a per-frame pointer map and spill
   pointer locals into a described frame, so the collector can follow `sl` and
   the frame. Confirm how `ORG.Mod`/`ORB.Mod` describe a frame, and whether the
   Project Oberon book's collector chapter says so (*not yet verified*).
   (b) Conservative: scan the C stack for words that look like heap pointers
   (Boehm-style). This fits a descriptor-driven *heap* and works without frame
   discipline, at the price of pinned objects and hidden bugs. (c) Explicit
   frames for every procedure (extend today's nested-only `sl` scheme).
   Recommend one with numbers: code size, allocation-path cost, and its effect
   on `-Ochecks` and traps.
2. **Descriptors**: add size and the pointer-offset list. Include arrays:
   is the element type a pointer, and how is the run-time length of open arrays
   represented? Mirror the reference layout so the code is recognisable.
   An imported type's layout must reach the client's collector, so this is also
   a **`.sym` format** question (W3.2). The format must be stable across modules.
3. **Trigger and policy**: on allocation failure, `rt_new` collects and retries.
   If allocation still fails, it returns `NIL`, as DESIGN section 8.6 specifies
   for failed `NEW`. Add a private `rt_collect()` for tests, a growth policy, and
   an `allocated` counter asserted at exit as a free leak check (the reference
   keeps one).
4. **What it breaks**: `SYSTEM` (W4.1) can hide an object's only reference in
   an `INTEGER`, so a *precise* collector is unsound for such programs. Document
   the convention (Wirth's systems have the same caveat) or use a conservative
   root scan. This paragraph originally said the sanitised suites would have to
   run with the collector **off** because ASan cannot see inside a
   collector-managed heap. **W10.2 showed otherwise**: ASan keeps locals in a fake
   stack (`detect_stack_use_after_return`), so a conservative walk misses every
   local and silently frees live objects. The collector had to be fixed; the
   runtime turns that option off and unpoisons the region it reads. The stress
   blocks build with sanitizers **on**, which exposed the defect.
   Section 15's ASan note records the resulting contract. Moving/compacting and
   generational collectors are out because they invalidate addresses that
   `ADR`/`PUT` may publish. The study should state this constraint directly.
5. **The harness already exists**: allocation-heavy generated programs
   (W6.1), collection at every `NEW`, and the `allocated`-counter assertion
   provide a stress test without writing one.

Method: read the reference sources and book chapter, plus VOC's tracing collector
and finalizer list (the survey in W8 found both). For the conservative option,
read Boehm's paper. Then write a new DESIGN section next to section 8.6 (which
currently says `NEW` is `malloc`), with a tradeoff table and recommendation.

Done when: that section answers all five questions with a source per claim.
Section 13 records the decision (adopt a design for W4.2, or leave GC out with
the study as the record). The *cost* is enumerated per code path (allocator,
descriptor, one safepoint per `NEW`, frame handling). W4.2 then references the
chosen design rather than the open question. This item does not code anything.
W4.2 covers that work.

---

## W5: error recovery

**W5.1 declaration-level recovery** (section 5 D7): **done**
Why: a missing `;` between two `CONST` declarations currently produces one located
message and stops. Attempts used four grammar shapes. Each was rejected with a
local ambiguity on the error token because the report lets each declaration
section be empty.
Options outside the current grammar:
1. expose the declaration non-terminals as their own LALRPOP entry points
   (`pub ConstDecl` ...) and let a driver-side loop parse `;`-separated chunks;
2. hand-write the declaration-section parser and keep LALRPOP for statements and
   expressions.
Done when: a missing `;` in a declaration list still reports errors *after* it,
with a test that fails today. **Done**, by option 1: `ConstDecl`, `TypeDecl` and
`VarDecl` are public LALRPOP entry points. The driver reads a broken section one
chunk at a time. It lexes, cuts at a `;` at nesting depth zero (a `;` inside a
`RECORD` or parentheses is not a separator), then asks each chunk's parser.
Only bad chunks report anything. The scan runs only when the module parse has
already failed, so a clean parse pays nothing and cannot be second-guessed. The
test `a_broken_declaration_list_still_reports_what_follows` fails without it:
one missing `;` in a `CONST` list and one in a `VAR` list now produce three located
errors (`unexpected `b``, an incomplete declaration, `unexpected `y``) instead of
the first alone. Boundary: this reports *diagnostics*, not an AST. The module parse
is still fatal there, so nothing after it is type-checked. That is the price of
option 1 over a second, hand-written declaration parser.

**W5.2 cascades, an error limit, and the baseline that pins them**: **done**
Why: W5.1 is the *hard* half of recovery. LALR(1) will not let us resynchronise in
declarations. The cheaper half had not been measured. The measurement uses one
small module per defect class, run through `obc check`:

| probe | diagnostics today | verdict |
|---|---|---|
| `TYPE T = Unknown; VAR a: T; BEGIN a := 1 END` | **1** | already right: the failed type poisons `a`, so the assignment says nothing |
| `CONST c = Zz;` then `TYPE T = ARRAY c OF ...`, plus two statements using `c` | **1** | already right, same reason |
| `r.m := 1; r.k := 2; r.n := r.m` on a record without those fields | **3** | already right: one per missing field; reading `r.m` does not add a second error to its assignment |
| one file with six defects (two `x := ;`, two uses of an undeclared `Zz`, plus clean statements) | **5** | right: no diagnostic is a *consequence* of recovery, and syntax errors do not hide semantic ones |
| **three uses of the same undeclared name** (`Zz := 1; Zz := 2; x := Zz`) | **3** | **fix: report the first use and note the rest**. This is the one repetition the checker still produces |
| **sixty broken statements in one body** | **60** | **fix: an error limit**. Nothing stops a flood |

### the commonest defect: a missing `;`

We measured every site where a separator can go missing, along with the `;`-first
message work that came out of it **[landed]**:

| site | today | after the message work |
|---|---|---|
| between statements in a body, next starts with an identifier (`x := 1` then `y := 2;`) | 2 diagnostics: a syntax error, then a *real* error after the skipped statement. Recovery **discards the next statement**, so errors in that statement are never reported | the notes now read together: `expected ;, END, DIV, MOD, OR or IN (12 other token(s) would do as well)` and ``the parser skipped `y := 2` to carry on; a later error may be a consequence``. The first names the missing token; the second identifies the innocent statement that was lost |
| between statements, next starts with a keyword (`x := 1` then `IF ...`) | same shape; the keyword makes it unambiguous to a reader at a glance | same |
| before `BEGIN` (`VAR x: INTEGER` then `BEGIN`) | one fatal error: recovery cannot resynchronise in declarations (W5.1) | `expected ; or END` now leads with `;` |
| between `CONST`/`VAR`/record-field/formal-parameter declarations | one fatal error each | same, with `;` leading |
| between procedures (`END P` then `PROCEDURE Q;`) | one fatal error | same |
| in an import list (`IMPORT Texts` then `Oberon;`) | one fatal error | same |
| between case arms (a missing `|`) | one fatal error | same |
| an *extra* `;` between declarations (`VAR x: INTEGER;;`) | one error (`unexpected ;`) | same |

The investigation identified these points, two of which changed the compiler's messages:

1. *The grammar cannot keep the innocent statement.* Letting the error token act
   as a statement separator (`<s:Statement> <e:!> <rest:StatementSequence>`)
   produces **4 LALRPOP conflicts**, the same wall as W5.1. A missing `;` in a
   body is *reported*, not *repaired*. The recovery note now says exactly what
   was discarded. **[landed: `git diff` of `crates/syntax/src/oberon.lalrpop`
   is empty; the experiment was reverted]**
2. *A diagnostic used to hold one note.* `Diagnostic::with_note` **replaced** the
   note, so a recovered error showed the recovery note and dropped the expected-token
   note. The reader lost "what would have fitted". `Diagnostic` now has
   `notes: Vec<String>` (as DESIGN section 10 specifies) and renders every one.
   **[landed; `several_notes_are_all_rendered`]**
3. *`;` leads the expected list.* LALRPOP provides the set in its own order, so
   the commonest defect used to be answered with "expected DIV, MOD, OR, IN...".
   `;` is now first when it would have fitted. **[landed;
   `a_missing_separator_is_reported_with_both_notes`, and the e2e check pins it]**

Both fixes are in place, and the item is closed:

1. *First-use suppression for a name*: **landed**. The checker reports an
   undeclared name once and counts later uses on that first diagnostic
   (``= note: 2 further uses of `Zz` are suppressed``). Repeated uses of one name
   now produce one diagnostic. Each name gets its own diagnostic. The test
   `one_mistake_is_reported_once` (`one_mistake_is_reported_once`) asserts both,
   including the double-lookup case that started it.
2. *An error limit*: **landed**. `--ferror-limit N` (default 20, `0` = no limit)
   works in both CLIs. Warnings are never dropped and the exit code is unchanged.
   Below the limit, behavior is unchanged: small e2e cases still assert exact
   counts; the flood case asserts the ceiling and suppression line; and
   `zero-means-no-limit` proves `0` means no limit.

This item deliberately omits declaration-level resynchronisation (W5.1, which
the grammar cannot express), a second parse pass after a fatal module-level error
(one located message is honest and cheap), and "reparse the tail" machinery
(the probes above show the simple behavior is already accurate).

Test cases are half the point of the item:
* the three tests landed above (`a_missing_separator_is_reported_with_both_notes`,
  `a_missing_separator_in_a_declaration_names_what_followed`,
  `several_notes_are_all_rendered`) cover messages; pinning the *counts* for every
  row of the two tables remains open;
* the counts are pinned where they matter: repeated-name rows and the flood row
  are tests now, the missing-`;` sites are covered by the three message tests, and
  the poison rows are covered by `a_failed_type_or_constant_poisons_its_uses`.
  A wider `recovery_tests` table, one row per site as `(source, expected (line,
  column, kind))`, may help if the checker's range of failures grows.
* **a shrinker**, now the binding constraint on the residue: `--per-statement`
  localises a mismatch to one statement, but that statement can sit inside a
  procedure of 25 nested loops, calls and `CASE` arms. `obgen --shrink SEED` should
  delete statements and simplify expressions while the mismatch persists (each
  candidate re-run), producing a minimal case like the residue's first finding,
  `LSL(1, 32)`.
* `e2e` checks assert a *ceiling* rather than an exact count for messy classes
  (the sixty-statement flood and six-defect file), because the ceiling is the
  property that matters.
* the generator's second job (W6.1 already exists): inject exactly one defect
  into a valid program: delete a `;`, replace an identifier with an undeclared
  one, or change a declaration's type. Assert four things: no panic, at
  least one diagnostic, at most K diagnostics, and the *first* diagnostic within
  a few lines of the injection. The last two detect cascades.
* tests that the poison behavior is intentional, not incidental, landed as
  `diag_tests::a_failed_type_or_constant_poisons_its_uses` (a bad `TYPE` and bad
  `CONST` each produce exactly one diagnostic; two missing-field reads produce
  two, with no follow-on error on either assignment), plus
  `the_error_limit_truncates_errors_and_never_warnings` for the limit itself.

Done when: tests pin the table, `-ferror-limit` exists and is tested in both CLIs,
the defect-injection mode runs clean over a few hundred seeds, and DESIGN section
10 states the policy (what is suppressed, the poison rule, and the limit's default),
since that section currently describes only rendering.
**Status**: everything except the defect-injection mode (W6.1's generator work)
is done; section 10 states the policy; both CLIs take the flag; 18 `diag_tests` and
29 e2e checks pass.


---

## W6: the random program generator, and what it found

**W6.1 the generator** (implemented in `crates/obc/src/bin/obgen.rs`; run with `bash fuzz/run.sh`)
Why: the three hand-written suites only test cases their authors considered. A
generator writes programs known to be well formed and to compute a value, so any
disagreement indicates a bug rather than a judgement call.
How it avoids being circular: `obgen` builds a small IR, renders it as Oberon-07,
and interprets the same IR with its own arithmetic, so the expected value of
every module variable comes from the generator, not from the compiler. The
emitted module ends with one check per variable:
`IF m0 # 42 THEN fail[1] := 0 END;  (* check m0 expected 42 *)`
The `fail` variable is an `ARRAY 1 OF INTEGER`, so a mismatch traps with the
source offset of the designator (DESIGN section 9.4). The program produces no
output. Optimization cannot remove the checks. A seed reproduces a program
exactly on any machine.

Covered today: `INTEGER` variables and constants, `+ - * DIV MOD`, `ABS`, the
report's shifts (`LSL`, `ASR`, `ROR`; there is no `ROL`), relations as
conditions, assignment, `IF`, `WHILE`, `REPEAT`, `FOR`, `CASE`, calls to
generated procedures with value parameters, and one recursive function.
Everything is inside the fragment where the report's semantics are unambiguous;
overflow is included on purpose, because `-fwrapv` makes wrapping the specified
behaviour, while `DIV`/`MOD` by zero and `MIN DIV -1` are excluded.

Not covered yet: `REAL`, `CHAR`, `SET` and `BOOLEAN` variables, arrays, records,
pointers, open arrays, `VAR` parameters, nested procedures and static links, type
guards, modules and imports, a shrinking pass for a failing seed, runs long
enough for the gate (W6.2 has to be fixed first), and a `SYSTEM` fragment whose
`PUT`/`GET` round trips and `SIZE` calls the mirror can predict (W4.1).

**W6.2 the first run: the emitted C is not warning-clean** (done)
Why it matters: the documented build line (`-std=c11 -O2 -fwrapv -Wall -Wextra
-Werror`, DESIGN section 9.1) is the guarantee the e2e suite tests. If a *legal* program
provokes a warning in emitted C, that is a compiler bug. Evidence: all 25 seeds
failed in the C compiler, not the front end. The checker accepted every generated
program. There were three distinct causes:

1. `-Werror=overflow` (106 messages). A constant subexpression that overflows is
   emitted as C constant arithmetic: seed 12 emits
   `(-((0 - 2147483647) - 1))` for `-(MIN(INTEGER))`. C computes the wrapped
   value, so this is a warning rather than a miscompile. The contract is that
   emitted C compiles. Fix: fold constant subexpressions before emission (best),
   or add `-Wno-overflow` to the build line, which the design can defend because
   `-fwrapv` already declares wrapping intended.
2. `-Werror=unused-parameter` / `unused-but-set-parameter` (13 messages). An
   Oberon parameter the body never reads is emitted as an unused C parameter.
   Existing suites happened to miss this case. Fix: emit `(void)p;` for a
   parameter the body does not read.
3. `-Werror=tautological-compare` (1 message). A comparison of two constants is
   emitted as a C comparison. Fold the operands as in (1).

Done when: `bash fuzz/run.sh` passes for a few hundred seeds and joins the gate,
with one regression fixture per cause in `e2e/` or a row in `Regress.mod`.
**Done**: `Regress.mod` prints the constant overflow (`2147483647 * 2`).
`e2e`'s `unused-entities-still-compile` builds a module with an unread variable,
parameter and procedure. Tautological comparisons use `-i > -i` and
`(i + 1) < (i + 1)`. `ABS(i) = ABS(i)` tests a self-comparison with calls as operands.
The seventh cause, gcc proving a *symbolic* overflow,
`(MAX - m2) + (MAX + m2)`, cannot be found by value folding. The documented
build line uses `-Wno-overflow`, justified like `-fwrapv` in section 9.1.
Evidence: `fuzz/regressions.txt`'s nine seeds, seeds 1..100, 200 seeds from
1000, and 400 from 5000 all pass. `e2e` has 55 checks. **Progress**: the fold,
parameter marks, and named-`CONST` lookup were added. The results changed:

| state | C rejections | value mismatches | passing |
|---|---|---|---|
| first run | 25 | 0 | 0 |
| after the fold and the parameter marks | 0 | 24 | 1 |
| after W6.5 (the shift semantics) | 0 | 23 | 2 |
| after the named-`CONST` fold and the self-comparison fold | 1 (a self-comparison, now folded) | 22 | 2 |
| after the mirror's `FOR` semantics was corrected | 1 | 3 | **21** |
| after the review of those commits (fold limits, `rt_abs`, the universe scope, initialise-with-literals, `-O2`) | 0 | 3 | **22** |

The result jump points to a generator bug. Section 9.8 defines
`FOR v := beg TO end ... DO S END` as equivalent to
`v := beg; WHILE v <= end DO S; v := v + inc END`. The control variable ends
at `end + inc`, as the emitted C `for` header leaves it. The mirror stopped at
the last body value, so programs reading the variable after the loop disagreed
with the compiler. `--explain` (`obgen --explain VAR --base SEED`, tracing one
variable after each top-level body statement) exposed the issue. The mirror's
value matched the expected value inside a `CASE` arm that ran a `FOR`, but the
control variable appeared in the following expression.

The remaining three seeds trap at their self-checks. Their divergence is inside
recursive function `R`: `m0 := m0 + R(3)` changes the variable whose check traps.
`--per-statement m0` narrows the mismatch to that statement. The mirror's `R(3)`
is 0, while the compiled value differs. `F1`'s body has 25 lines of nested
loops, calls and `CASE` arms. Further reading has not helped; the next step is a
shrinker (below).

**W6.5 the generator found a real bug: `LSL`/`ASR` masked their count** (fixed)
Why it matters: the report's table (section 8.2.2) defines `LSL(x,n)` as
`x * 2^n` and `ASR(x,n)` as `x DIV 2^n`. Counts of 32 or more are defined: 0
for logical shift left, 0 or -1 for arithmetic shift right. Only rotation uses
modulo 32. The runtime helpers, constant folder, and `csmoke` spec used the
wrong rule, so `LSL(1, 32)` returned 1 instead of 0.
Evidence: a generated program's self-check trapped. The generator's mirror
follows the report, so this was a compiler mismatch. `LSL(1,32)` alone also
shows the error.
Fixed in `crates/cgen/src/oberon_h.inc` (both helpers), `crates/sema/src/const_eval.rs` (both folds and
its doc), `csmoke/smoke.c` (the copied helpers, the case that encoded the masking,
and four new cases), `golden/lower.c` and `golden/constexpr_check.c` (the
second-pass recomputation), `golden/constexpr.tsv` (regenerated), and the four
document mentions. DESIGN section 13 R22 records the fix. The suites passed
afterwards. The hand-written suites had all agreed with the wrong rule.

**W6.3 language details the generator had to learn** (recorded, not a compiler bug)
The first runs also tested the *generator*. Located diagnostics made each issue
clear. These details are easy to miss in a hand-written corpus: identifiers
have no `_` (report section 3). `REPEAT ... UNTIL expr` has no `END`. A case
label is `integer | string | qualident`, so it cannot be negative. `CASE` has
no `ELSE`. The default arm has an empty label list (section 10). `MIN`, `MAX`
and `ROL` are not predefined procedures (section 10.2). DESIGN section 13 used
`MAX(INTEGER)` in three places. That prose is now corrected.

**W6.4 a gap the generator could not work around** (decided: the deviation is
recorded in DESIGN section 5 D8)
The report's `case = [CaseLabelList ":" StatementSequence]` allows a default
arm written as a bare statement sequence, `| S`. Our grammar requires labels
and `:` together. `CaseArm = labels? (":" body)?` cannot be extended to
`| labels? ":" ? body` without a conflict. A statement sequence and a label
list share a first set because a procedure-call statement starts with the same
qualident a label may use. LALR(1) cannot look two tokens ahead; the reference
compiler's recursive descent can. Options: a hand-written case-arm parser, a
two-token lookahead trick, or documenting the deviation and requiring the
empty-label-arm spelling `| END`. Doing nothing is also defensible because no
Oberon-07 program needs a default arm, but DESIGN section 5 records the choice.
**Decided and recorded**: the grammar keeps `CaseArm = labels? body?`. The
default arm is `| : S` (`| : S` is accepted; `| S` is rejected with a
diagnostic naming the missing colon). DESIGN section 5 D8 explains the conflict
and the choice.

---

**W6.6 the generator's three standing failures** (done)
`fuzz` reports three failing seeds (7, 9, 17 of 25). Each program's self-check
traps because its computed value differs from the generator's expectation. The
check uses the out-of-range index `fail[1] := 0` to trap at a known source
offset. None of the seeds uses an open array or `NEW`, so W7.4's fixes are
unrelated. Their arithmetic uses the shortcut family (`F0`, `LSL`, `ASR`,
`DIV`, `ABS`) touched by R22. The model must use `LSL(x,n)=x*2^n` for counts
at or beyond the word size. A masked model gives a wrong expectation.

**Done**: fixed in the *compiler*. DESIGN R23 records the reason and section
9.5 the mechanism: operands and arguments are evaluated in source order. Seeds
7, 9, and 17 are in `fuzz/regressions.txt`, along with seeds 1098 and 1116 that
found the argument case.

Progress, by instrumenting seed 17 (the module's checks replaced with
`Out.WriteInt` of each variable, so the program's values could be compared with
the generator's):
* Of twelve checked variables, **one** differs: the program computes `m0 = 5`
  where the model says `3`. Everything else agrees, narrowing the divergence to
  one chain of statements rather than the generator's arithmetic at large.
* The generator's *model* of shifts follows the report: `LSL` and `ASR` of 32
  or more are 0 or -1. A C probe of `rt_lsl`/`rt_asr` gives
  `LSL(1,33)=0`, `LSL(1000,33)=0`, `ASR(-1000,33)=-1`, `LSL(3,32)=0`,
  `ASR(-8,2)=-2`, and `LSL(1,31)=MIN(INTEGER)`. R22's semantics are intact on
  both sides, so shifts do not explain this failure.
* The last statement that writes `m0` is `m0 := (m0 + R(3))`, the shape DESIGN
  R23 records. Before it, `m0 := (F0(F0(MIN)) + LSL(F0(MAX), 33))` calls `F0`
  in both operands. `F0` writes module variables (`m0`, `m8`) and contains a
  `REPEAT` and `CASE 1 OF 1:`. R23's minimal reproduction does not fire at any
  `-O` level, so the suspect is inside `F0`, not the order of `+`.
* Next, instrument `F0`'s body (print `p0`, `acc` and module variables at each
  iteration) to distinguish an emitter error in `REPEAT`/`CASE` from an operand
  evaluation-order error.


## W7: memory safety: evaluate it, then test it

Why: the design promises that every memory access in emitted code is either
provably in bounds by the type system or guarded by a trap naming the source
offset (sections 9.4 and 8.6, R14). `SYSTEM` is the deliberate escape. This
promise had not been audited end to end. R20 showed the risk of assuming it
rather than testing it. Probing found two holes (W7.1, W7.2) and confirmed two
static checks.

**What is answered today** (probes, all in `fuzz/findings/` style modules):

| question | answer |
|---|---|
| an out-of-range *constant* index, `a[10]` on `ARRAY 4 OF INTEGER` | accepted at compile time, traps at run time: `index out of range (at byte 62 of the source)` |
| an out-of-range *variable* index | every index goes through `rt_index(i, n, at)`, including constants. There is no static bounds analysis. |
| a **plain dereference of `NIL`** (`p.x := 1`, `p^ := p^`) | **nothing**: no check is emitted, so the process dies of SIGSEGV with no message. See W7.1. |

**W7.2 a guard on a VAR record parameter of an *imported* type** (open, found by
`e2e`'s mixed-back-end rows while resolving the layout question)
Why: reclassification of `b(T0)` (guard or call, D2/D3) works for a parameter
whose record type is local, but not for an imported type. The compiler refuses
the legal way to reach an imported type's descriptor:
`PROCEDURE P*(VAR b: Def.Base); BEGIN b(Def.Ext).x := 1 END P` reports "a designator
cannot be applied twice: the result of a call cannot be selected from" (section 8.1).
The same procedure with `Base`/`Ext` declared locally checks clean. Mistaking a
guard for a call is worse than refusing it, so this is its own item. Add the
shape to `e2e` and remove the refusal.
Done when: the procedure checks and runs, and its guard reads the imported
type's descriptor.

**W7.1 a plain pointer dereference is not `NIL`-checked** (done)
Why: `rt_trap`'s comment in the emitted header lists its reasons as
"bounds, NIL, case, ASSERT". Section 9.5 checks `NIL` on the *guard* path, but
the dereference lowering (`Mem3__p->x = 1`) had no check. Evidence:
`p := NIL; p.x := 1` exits 139 (SIGSEGV) with empty stderr, while other
violations in this class report a located trap. `p^ := p^` does not crash;
the C compiler may do anything with it, which shows the same problem.
Fix, in `crates/cgen/src/cgen.rs`: lower a pointer access through a `NIL` test.
Use section 9.5's comma-expression trick for reads:
`(rt_nil(p, "... at byte N"), p->f)`. For an *lvalue*, hoist a statement:
`(rt_nil(p, ...), p->f) = x` is invalid C. Add an `rt_nil` helper beside
`rt_guard_is`. Decide whether to check every dereference (one trap per access)
or only pointers not provably non-`NIL`. We have no such analysis. Section 13
records the decision.
Done when: both field and `^` dereferences trap with the designator's offset.
The e2e trap matrix includes both spellings. Emitted C for the guarded case
must compile under the documented build line. **Done**:
`rt_nil` is a macro whose result keeps the operand's pointer type (`void *`
cannot be selected from). `rt_nil(p, at)->f` also remains an lvalue, so
assignment to a pointer target's field compiles. Both spellings trap with the
offset (`e2e` checks `nil-field-dereference-traps` and
`nil-caret-dereference-traps`). An emission test checks that dereferences have
the check and pointer assignments or `NIL` tests do not. Every dereference
with a side-effect-free operand is checked. **Remaining**: side-effecting
operands, such as `a[Next()].x`, are not checked. `rt_nil` reads its operand
twice, which could change the selected element (DESIGN R26). The check is
omitted rather than emitted incorrectly. To finish, hoist the operand into a
statement using the emitter's temporary machinery (R16), following the fold's
call rule.

**W7.2 a constant out-of-range index is not diagnosed** (done: warn, and still
trap)
Why: `a[10]` on `ARRAY 4 OF INTEGER` compiles, and `e2e/Trap.mod` relies on
that: its comment says the check must trap and report the location. Runtime
behavior is correct. The check traps with the offset. A compiler can also warn
(`rt_index(((int32_t)10u), 4, 62)` appears in emitted C), so the folder knows
the value and length.
**Done** as decided: the compiler warns (not errors) when the folded index is
outside `0 .. LEN-1`. `Trap.mod` includes the warning before its trap.
`diag_tests::a_constant_index_out_of_range_warns_and_still_traps` pins both
the warning and emitted check. The compiler reports what it can see. The check
still traps, and both behaviors are tested. Wirth's compilers do not diagnose
it. Section 13 should record that choice. The check's runtime behavior and a
diagnostic for a known-invalid index are separate promises.

**W7.3 the audit (the main deliverable)** (done)
Why: "every access is safe" is a claim about every place emitted C touches
memory. DESIGN section 9.4 needs a table with the safety mechanism and test
The list covers fixed and open-array indexing, record fields through an
extension or guard, and pointer dereferences (`^` and `.`), including `NIL`
(W7.1). It includes value-parameter copies, whole-array or record assignments
(`rt_rec_copy`, `rt_strcopy`, R14's exact-length case), string-literal
initialisation, frames and static links (R7), descriptor reads for type guards
and type-`CASE` (including stale or corrupted `id`), `NEW`, and failing `malloc`
(R3). The table also covers the trap path.
Done when: each row names a test and explains safety through the type system
or a helper visible in emitted C. **Done**: DESIGN section 9.4 has twelve rows
from array indexing through the trap report. It also records two exceptions:
`rt_new`'s failure path still lacks a test (W7.4's allocation hook). The NIL
check is also omitted for a side-effecting operand (R26).

**W7.4 the trap matrix in `e2e`** (done; it found two compiler defects)
Why: each deliberate violation needs to assert its reported *offset*. A trap
with the wrong location is nearly as bad as no trap.
Each test has its own module because the first trap aborts the process. Cases:
index below 0, index at `LEN`, index in an open-array parameter, failed record
and pointer guards (R15's two spellings), `NIL` field and `^` (W7.1), type-`CASE`
with no match, `DIV`/`MOD` by zero, and failing `ASSERT`. To test failed `NEW`,
use the existing `rt_new` hook (R3) with an `OB_ALLOC_FAIL_AT=n` environment
variable.
Done when: the gate runs the matrix and each row asserts its expected offset,
not just that it trapped. **Done**: eleven rows cover three index shapes
(below zero, at `LEN`, and in an open-array parameter), both guard spellings,
type-`CASE` with no match, `DIV` and `MOD` by zero, failing `ASSERT`, two `NIL`
dereferences, and failed `NEW`. The `OB_ALLOC_FAIL_AT` hook makes the report's
specified allocation failure testable. Each module checks both message and
offset.

The matrix found two real defects no other suite saw (DESIGN R27, R28): a `VAR`
open-array parameter was dereferenced as if it were a pointer to the array, so
emitted C did not compile; and open-array indexing had no bounds check against
the hidden length. Both are fixed.

One row is narrower than it looks: a *failing* guard that reads the guarded
field cannot compile under `-Werror`; gcc sees the read is outside the
allocation (R15). The matrix's guard rows use the guard as a value and its
pointer spelling. R15 records the limitation.

**W7.5 the generator as a memory-safety oracle** (done)
Why: W6.1's self-checks verify *values*. They cannot detect a wrong length, a
stale frame pointer, or a descriptor read that happens to land inside the heap.
Use the existing machinery in two ways:
1. Run generated programs compiled with `-fsanitize=address,undefined`
   (`obc build --cflag`, as the golden suite already does twice). A clean run
   then gives evidence about emitted code, not the program's arithmetic.
2. Add an adversarial mode: emit programs that deliberately violate a condition,
   such as an index past the end, a `NIL` dereference, or a guard that cannot
   hold. The mirror records that a trap is due instead of computing a value.
   Assert that the program traps at the expected offset without corrupting
   anything. This exercises the guard paths as well as the value paths.
Done when: a few hundred seeds are ASan/UBSan-clean and the adversarial mode's
traps report their expected offsets. **Done**: `SAN=1` builds every generated
program with `-fsanitize=address,undefined -fno-sanitize-recover=all` and the
sanitizer environment the golden suite uses. Three hundred seeds from base 2000
and the nine regression seeds are clean, with leak detection on. `--adversarial`
appends one deliberate violation per program after the value checks, so a wrong
value still fails first: an index one past `arr`'s end, `pa.x := 1` with
`pa = NIL`, a guard that asks for a pointer to the extension while the object
is the base, a divisor computed to be zero (DIV and MOD), and `ASSERT(FALSE)`.
The generator renders the violation and computes its expected trap line,
including the byte. The harness compares that line, not just whether a trap
occurred. A mutation adding five to the expected byte reports
``expected `... at byte 3655 ...`, got: ... at byte 3650 ...``. Two hundred seeds from
base 3000 and the nine regression seeds pass. The standard suite runs the
regression list in three modes (values, traps, and sanitizers), then 100
coverage seeds and 25 adversarial seeds.


---

**W7.6 the narrowing must not cast the designator** (done)
A guard or type-`CASE` arm narrows a designator's C type. That cast makes it
stop being an lvalue, so `p := NIL` inside `CASE p OF PS: ... END` did not
compile (DESIGN R29, found by W7.4). The matrix's empty arm avoided the error,
the kind of test accommodation the matrix was meant to remove.

Done: stores use the plain spelling when the narrowed name is the whole
designator. `field_path` walks down the base chain and receives the derived
record. If a selector follows the name, the cast must remain for the field
offset to be correct. The emitter keeps the original type for coercions. The guard check is
hoisted to a statement in lvalue position, flushed by the assignment emitter
and builtins that take a `VAR` argument. A whole-designator guard also drops
its cast. Tests:
`narrow-assign-to-narrowed` (assignment to the arm's variable),
`narrow-field-through-narrowed` (field write and read through the arm's
variable), and `narrow-guard-as-target` (store through both guard spellings,
record copy from a guarded view, and narrowed pointer passed to a `PS` parameter).

## W8: what production Oberon compilers add, and what to adopt

The survey covered Wirth's own compilers (Project Oberon 2013's RISC compiler `ORP`/`ORB` and `OP2`), **VOC** (Vishap Oberon), **Oberon+** (`obx`/`obc`), small-target compilers (AntKrotov's Oberon-07 for MSP430 and STM32, and Oberon-07 for CH32V003/RP2040), and production dialects (Oberon-2, Component Pascal, Active Oberon, Zonnon). For each, I checked what it adds beyond report section 10.2, whether it is *additive* (a report program still compiles and means the same), and the primary source. Nothing here is adopted yet. Each item is a proposal with a cost.

The survey corrected one row on these points:

* The report's grammar has no `ELSE` in a `CASE` statement: `CaseStatement = CASE expression OF case {"|" case} END`, `case = [CaseLabelList ":" StatementSequence]` (report, appendix). Our parser rejects `ELSE` **[verified]**.
* Adding `ELSE` as a final clause is conflict-free in our LALRPOP grammar and reuses the existing default arm (`labels: None`) **[verified by experiment: 0 conflicts, and probes with and without `ELSE` both check clean]**.

**W8.1 `CASE ... ELSE`**: **done**
The surveyed dialects that include it are Oberon-2 section 9, Component Pascal
section 9.11, Oberon+ Case statements, and Zonnon. AntKrotov lists it among the
documented differences from the original. It closes **W6.4**. LALR(1) cannot
express the report's default arm, an empty label list followed by a statement
sequence (`| S`), because statements and labels share a first set. `ELSE` can
express that arm. `ELSE` already appears in `IF`, so it reuses that keyword.

**Done**: `ELSE` uses that same node. Multiple defaults in one `CASE` are
refused (DESIGN section 8.5 row 63). The compiler does not order them. `ELSE`
makes this reachable because a label-less `|` arm may precede it. Tests:
`case_else_is_the_default_arm_and_there_is_only_one`, and `e2e`'s
`min-max-and-case-else` and `two-case-defaults-are-refused`.

**W8.2 `MIN(T)` / `MAX(T)`**: **done**
Report section 10.2 has no `MIN` or `MAX` **[verified in the report's own table]**.
The language needs a way to name an extreme. The folder and generator had to
spell `-2147483648` as `(0 - 2147483647 - 1)`. DESIGN prose assumed
`MAX(INTEGER)` in three places until R20's round of corrections. AntKrotov's
compiler lists `MIN`/`MAX` among its additional standard procedures.

The change adds two entries to the predefine table. It takes one *type* argument,
using the same mechanism as a type guard (section 5 D2), and it adds folding.
A declaration of `MIN` still shadows the predeclared name under existing
scoping rules. **Done**: `MIN`/`MAX` are predeclared *names* dispatched by the
checker. It records each value at the call's span, the channel `SYSTEM.SIZE`
uses for its type. The emitter lowers the call to a constant. A constant
*declaration* folds it like other constant declarations. The resolver keeps the
parenthesized group a call because its argument is a type. The procedures are
defined for `INTEGER`, `BYTE` and `CHAR`. A type without an extreme is refused
instead of given a zero (rows 61-62). Tests: `min_and_max_are_constants_of_ordinal_types`,
`a_declared_min_shadows_the_predefined_one`, and the two `e2e` rows.

**W8.3 per-check selection for runtime checks**: **done**
AntKrotov's `-nochk` disables pointer, guard, index, `INTEGER`-to-`BYTE`,
`CHR`-range and stack-overflow checks *individually*. Our plan (W3.1) has only
the all-or-nothing `-Ochecks`. Per-check switches let the memory-safety work
(W7.4) vary checks per row, at the cost of a flag. This changes only the
compiler surface.
**Done**: `-Ochecks=<list>` takes `index`, `nil`, `guard` and `case`. `none`/`n`
and `all`/`y` select an empty or complete set. A build can keep the checks under
test and drop the rest. The compiler refuses an unknown name. `e2e` pins the
behavior structurally: `-Ochecks=nil` emits `rt_nil` and no `rt_index`, and the
reverse, beside the all-or-nothing rows.

**W8.4 early `RETURN`**: **done** (DESIGN section 5 D6 is revised by it)
VOC, Oberon-2 section 10.1, Component Pascal section 10, Oberon+ and Zonnon
section 7.9 all allow `RETURN` inside a procedure body. D6 chose the report's
end-only form partly to avoid multiple result-assignment sites. That benefit is
weak in C, where an early return is `return expr;`, while the usability cost is
real: a procedure cannot return from inside a `CASE`. The proposal accepts
`RETURN` as a statement and keeps the accumulator shape for the report's spelling.
**Done**: the single-exit guarantee remains. `RETURN [Expr]` is a statement, and
a procedure with a result has one accumulator (`ret__acc`, zero-initialised) and
one exit. Each `RETURN` becomes `ret__acc = ...; goto ret;`. The emitter, not the
grammar, now ensures the property D6 documented. The report's tail clause is
removed because it would create lookahead ambiguity with the statement form. The
check now requires a function to *contain* a `RETURN` with a value. Test:
`exit-and-early-return` in `e2e` covers early returns from an `IF` and a `CASE`,
a `CASE` with no matching arm before a later `RETURN`, and a naked `RETURN`.

**W8.5 `EXIT` for loops**: **done**
`LOOP`/`EXIT` exist in Oberon-2, Component Pascal, Oberon+ and Zonnon. `EXIT`
alone removes the flag-variable dance from `WHILE`/`REPEAT`/`FOR` bodies and
maps to C's `break`. Wirth's 2007 revision removed features "judged duplicative
or non-essential" for "regularity, simplicity, completeness, and well-structuredness".
For this reason, `LOOP` (redundant with `WHILE TRUE`) is not proposed, and `EXIT`
should be weighed rather than assumed. `EXIT` is a new reserved word. An
identifier spelled `EXIT` in upper case would collide; lower case remains safe
because Oberon is case-sensitive. Oberon+ documents the same caveat for `WITH`
and `LOOP`. **Done**: `EXIT` lowers to C's `break`, leaving only the innermost
loop in each emitted form (`for (;;)` with guards inside, or a C `for`), so no
bookkeeping is needed. `EXIT` outside a loop is refused with a located error
(row 64). Tests: `exit-outside-a-loop-is-refused` and the `e2e` row covering all
three loop forms.

**W8.6 `LONGINT` / `LONGREAL` as real 64-bit types**: **deferred, decision recorded**
Oberon-2 section 6.1, Component Pascal section 6.1, VOC and Oberon+ (which goes
further with `INT8`...`INT64`) all have wider numeric types. Wirth's Project
Oberon compilers define `LONGINT`/`LONGREAL` as **synonyms** for `INTEGER`/`REAL`,
with no widening. A C back end can represent wider types as `int64_t` and
`double`, but they affect the type algebra, folder, emitter, `.sym` format and
generator's mirror. The corpus, suites and target programs do not need them, so
defer the change. The reference compilers use these names for synonyms. Adopting
them as wider types would conflict with that usage. Revisit when a program needs
one, then consider it as a dialect (DESIGN section 13 decision 13).

**W8.7 the escape hatch's scope**: **recorded, used by W4.1, which is done**
Surveys show `SYSTEM` growing in similar ways.
Wirth's RISC compiler adds `H`, `COND`, `REG`, `LDREG`, `LDPSR`, `LED`, `ADC`,
`SBC`, `UML`. VOC adds `INT8`...`INT64`, `SET32`/`SET64`, `ADDRESS`. AntKrotov adds
`GET8/16/32`, `PUT8/16/32`, `SADR`, `WSADR`, `MOVE`, `TYPEID`, `CODE` and, on STM32,
`WCHAR`/`WCHR`. W4.1 takes the report's portable five. It deliberately does not
take the *sized* accessors (`GET8/16/32`, `PUT8/16/32`): the report has none, and
the host-width byte-address model does not need separate entries. Register, PSR,
`LED`, `CODE` and `WCHAR` are target-specific and stay refused by name.

**W8.8 what the survey says to reject**: **recorded; closed**
* Syntax conveniences *other than the keyword case above* (W8.10) include `_`
  in identifiers, `//` comments, optional end names, single-quoted strings and
  conditional compilation (AntKrotov, Oberon+). Each makes a program
  non-portable to a report compiler. We value strict conformance.
* Type-width *models* include VOC's `-O2`/`-OC`/`-OV` and Oberon+'s fixed-width
  set. A dialect matrix multiplies the type algebra. The report fixes `INTEGER`
  = 32 bits and `REAL` = 32 bits.
* `WITH` is superseded by guards, `IS` and type-`CASE`. Wirth removed it.
* Type-bound procedures/methods appear in all four dialects. The report's
  `IS`/guards/type-`CASE`/procedure variables already cover runtime dispatch
  without a vtable model.
* Dynamic and multi-dimensional open arrays, `NEW(v, n)` and `LEN(v, dim)` occur
  in all four. They interact with descriptors and a tracing collector in systems
  that have one.
* Tracing garbage collection and finalizers appear in VOC and Oberon-2 systems.
  The report has no finalizer concept, so finalizers stay rejected. GC is not
  simply "out". Its design is being studied first (W4.5), with `rt_new` as the
  door (R3).

**W8.9 a lesson that is not an extension: our overflow stance is ahead of production**: **recorded; closed**
VOC's C back end does not establish `-fwrapv`, so signed overflow there is
undefined C. Our design fixes it deliberately (R4). R20 makes that choice
*warning-clean*, without giving it up.


**W8.10 lower-case keywords**: **done** (the keyword fold; predeclared-name aliases are the separate additive half, not taken)
What: accept reserved words spelled in lower case (`module`, `begin`, `if`, ...) so a program can be written in the host toolchain's taste; identifiers stay case-sensitive, so `Foo` and `foo` remain distinct.
Why: both surveyed compilers that support Oberon in a lower-case codebase do
this. AntKrotov's Oberon-07 lists "lower-case keywords" among its differences
from the original. Oberon+ accepts "lower-case forms of reserved words" and
documents the reserved-word collision as the cost. It also supports shorter
variants and omitted semicolons are excluded.

The default depends on whether a change is additive. W8.1-W8.5 leave report
programs unaffected, so they may be on by default. Folding keyword case changes
the report's distinction between capital and lower-case letters (section 3).
A report program that uses `Begin` or `integer` as an identifier would tokenize
differently. The flag remains off by default, keeping the compiler a strict
report compiler unless asked otherwise.

The change is local: keyword recognition uses one table and one comparison
(`KEYWORDS` and the lookup in `crates/syntax/src/lexer.rs`). `Lexer` has a
dialect field. `Lexer::new` keeps report behavior, while folded mode compares
with `eq_ignore_ascii_case` instead of `==`. The token stream remains
unchanged downstream, so the grammar, AST, checker and emitter do not need to
know about the dialect.

`INTEGER`, `CHAR`, `TRUE`, `FALSE`, `NIL` and the section 10.2 procedure names
are predeclared identifiers, not keywords. Their lower-case forms are aliases
in `sema::predefine`, not a lexer concern. User declarations shadow a
predeclared name, so aliases are additive and may be on by default. This is a
separate, inexpensive change from folding keyword case.
Cost and risk: the collision surface is "every identifier whose spelling matches a keyword case-insensitively". The diagnostic for a token where an identifier was expected must explain that the current dialect folds keyword case. Otherwise, the reader will not know why `begin` stopped being a name.

Tests: the existing near-miss table extended with a lower/mixed-case spelling per keyword; a negative test that the default dialect still rejects `begin`; a test that identifiers are not folded; a test that a module declaring its own `integer` or `nil` still compiles with the aliases in place; an `e2e/Lower.mod` built and run through the driver with the flag; and the generator gains a lower-case rendering mode, so the fuzzer covers the dialect end to end.
Completion required both CLIs to accept the flag and the default to remain
unchanged, as verified by the suites. Tests also had to check the collision
diagnostic, the interface header's dialect and W3.2 version line, and DESIGN
section 3.
both CLIs support `-Odialect=report|lower`, using `Lexer::with_folded_keywords`.
The interface writes `DIALECT <name>` only for a non-default dialect, so a report
interface stays byte-identical to before. Unknown names are refused. The collision
note names the dialect and does not fire for a reserved word spelled as in the
report, since the dialect does not explain that spelling.

Tests: `lower_case_keywords_need_the_dialect` checks that report mode rejects
`module`, `Foo` and `foo` stay distinct, and lowercase `integer` remains an
ordinary identifier. `a_folded_collision_explains_itself` checks the collision
diagnostic. The generator also supports lower-case rendering: `obgen --lower`
changes reserved words only, preserving predeclared identifiers and generator
names. It builds with `-Odialect=lower`, and `fuzz/run.sh` runs the regression
list that way on every invocation. The corpus now covers the dialect end to end
with the same values.

These proposals are all pieces of Oberon-2. The relevant question is whether to
support that dialect, rather than add each feature on its own. Oberon-2's own
report gives the following delta:

| Oberon-2 has, Oberon-07 does not | where | already planned here |
|---|---|---|
| `CASE ... ELSE` | `CaseStatement = CASE Expression OF Case {"|" Case} [ELSE StatementSequence] END` (section 9.5) | **W8.1, recommended** |
| `EXIT`, `LOOP` | statement list (section 9) | **W8.5, `EXIT` recommended** |
| `RETURN [Expression]` as a statement | statement list (section 9) | **W8.4, recommended** |
| `SHORTINT`, `LONGINT`, `LONGREAL`, `MIN`/`MAX` | sections 6.1 and 10.2 | **W8.6 deferred, W8.2 recommended** |
| `LEN(v, n)`, `HALT` | section 10.2 | not planned |
| `NEW(v, x0...xn)`, open arrays beyond formal parameters (pointer bases, element types) | sections 6.4 and 10.3 | W8.8 rejected |
| type-bound procedures: receivers, override on extension, forward declarations | section 10.3 | W8.8 rejected |
| type **inclusion** between numeric types (`LONGREAL` contains `REAL`, `REAL` contains `LONGINT`, `LONGINT` contains `INTEGER`, and `INTEGER` contains `SHORTINT`), i.e. implicit widening | section 6.1 | W8.8 rejected (it contradicts our own rule) |
| `WITH v: T DO ... | v: U DO ... ELSE ... END` | section 9.5 | W8.8 rejected |

The table shows that W8.1, W8.2, W8.4 and W8.5 are already recommended. Each
feature has its own justification and is additive, so adopting them does not
commit us to an Oberon-2 dialect.

The expensive changes involve semantics as well as syntax. `WITH` offers the
guarded view our type-`CASE` already provides through a type label and narrowed
designator (section 8.1, section 9.5). It is Oberon-2's *spelling* of machinery
we have. Numeric *inclusion* replaces our explicit "no implicit conversion"
rule with widening, touching the type algebra, folder and every compatibility
check. Type-bound procedures need a method table per descriptor (`ObDesc` is
already per type), override rules and super calls. Heap and nested open arrays
also interact with the collector study (W4.5).

The dialect mechanism still needs a decision. LALRPOP cannot switch alternatives
at run time. Supporting a dialect means maintaining two grammars or using one
permissive grammar with the checker gating what it allows. The checker owns
semantics and can issue a diagnostic naming the construct:
``WITH is an Oberon-2 construct; this module is compiled as Oberon-07 (see --dialect)``.
The interface file also needs a dialect field (W3.2) so clients know what they
are importing.

Scope boundary: this item is about the *language*. Eth Oberon's Oberon-2
*library and system modules* are a separate port, and VOC already occupies that
space. A concrete program to compile or an analysis need would provide a reason
to revisit it.

Recommendation: **do not adopt Oberon-2 wholesale now.** Take the additive pieces
through their own items as recommended. Record the mechanism decision above in
DESIGN so a future dialect is a checker-and-emitter job rather than a parser
rewrite. Re-open this item only for a concrete trigger, such as an Oberon-2
program or library we want to compile. Until then, leave `WITH`, methods,
inclusion and heap arrays declined (W8.8). `WITH` offers only a different
spelling for our type-`CASE` and costs a new reserved word.

Done when: DESIGN records the decision in section 5's dialect note and section 13.
Each overlapping item must remain individually justified, and this item must name
the trigger that would change the answer.

Done when: each item above has tests and is implemented, or is declined in DESIGN
section 13. DESIGN section 1 should point here so readers know the extension
question has been asked and answered.

---

## W9: a second back end that emits ELF directly, on Cranelift

**W9.1 research the back end, then build it beside the C one**: **done**
(the research is in DESIGN section 16, the prototype is in section 16's second subsection)

The integrator independently checked the author's report: `obc emit
Elf.mod --backend=clif` writes `Elf.o`, and `file` reads it as *ELF 64-bit LSB
relocatable, x86-64*, with machine code for the module and exported `Elf__init`
and `Elf__load`. Its only undefined symbols are `ob_rt_index_at`, `ob_rt_div_at`,
`ob_rt_mod_at`, `ob_rt_trap_pos` and `ob_register_commands`. This exposes the ABI
boundary in the object: the module is compiled, and the runtime stays C.
The corpus passes on both back ends (`bash fuzz/run.sh` and `BACKEND=clif bash
fuzz/run.sh`, exit 0 each). `BACKEND=clif bash e2e/run.sh` stops at the first row
the back end cannot yet meet: 39 check lines against the C path's 101. This
harness is for W9.2, not a failure of W9.1.

Why: W7 memory-safety and code-generation findings concern the emitted C, not
Oberon-07: R15 (a guarded read gcc can prove is outside the allocation), R20 (a
`malloc`-typed `NEW`), `-Werror` classes that make a correct program unwritable,
and the `-fwrapv`-style care the runtime needs. These issues arise because a C
compiler can assume the C is defined and changes its warnings between versions.
Cranelift's integer arithmetic wraps by definition. We emit traps by calling
`rt_trap`, so byte-offset diagnostics remain unchanged. A backend that writes ELF
also needs no `cc` in the build.

*The crates*, verified at crates.io on 2026-10-15: the family is `cranelift`
0.136.3, `cranelift-codegen` (the generator), `cranelift-frontend` (the IR builder),
`cranelift-module` (functions and data across modules), and `cranelift-object`
("Emit Cranelift output to native object files with `object`", the ELF path).
`cranelift-native` and `cranelift-control` are also used. All use
`Apache-2.0 WITH LLVM-exception`. Crates.io has no `cranelift-backend` crate.

*Research, before any code*: record each question and its source in DESIGN:

1. **The object path.** `ObjectModule` + `object::write`, and which `Symbol` and
   `Section` choices contain static data: per-type `ObDesc` objects, the command
   table (an array of function pointers) and generated `main`.
2. **The C ABI boundary.** The runtime stays C. Record `rt_*` signatures and
   `ObDesc` layout as a fixed interface. Confirm Cranelift's calling convention
   is the platform's C convention for our shapes (scalars and pointers only;
   records pass through pointers, never by value). Also check how a data object's
   address is materialised for `rt_new`/`rt_is`.
3. **Layout.** Cranelift's IR types are flat. Records are byte blobs reached
   through a pointer, so the C emitter computes their offsets. Confirm the IR
   builder does not require a second layout. The descriptor at offset 0 is part
   of the interface.
4. **Checks and traps.** Keep each check as an explicit branch to a runtime call
   (`rt_index`-style helpers or `rt_trap`). This avoids depending on a hardware
   fault and preserves traceback source offsets.
5. **Debug info.** Choose between DWARF (`cranelift-object`'s debug support,
   `gimli`) and traps as the only locator, as the design already argues.
6. **Integration.** Add a `--backend=c|clif` switch. Keep the C path as reference.
   `golden` and `csmoke` are C-level oracles, so they stay on C. `e2e` and `fuzz`
   are back-end-agnostic and become the Cranelift gate.
7. **What is lost as well as gained.** There is no preprocessor, no `-O2` beyond
   `cranelift-codegen`'s own optimisation levels, and no GCC extensions. The
   label-address idiom used by the C descriptor tables has no counterpart and
   is not needed.

*Plan*: put a lowerer beside `cgen` to consume the same checked AST and `sema`
types. It builds SSA per procedure with `cranelift-frontend`'s `FunctionBuilder`.
A `VAR` parameter or address-taken local becomes a stack slot; other values stay
in SSA. Emit through `cranelift-object`. Keep the runtime interface fixed so both
back ends link the same runtime and mixed builds work. This also lets the C path
serve as a comparison. Once the prototype is green, lower the last C dependency,
the runtime over libc, through the same back end. Only then will the whole program
generate ELF directly.

Done when: DESIGN records the research questions and sources, the prototype
builds and runs all `e2e` modules, `fuzz` seeds pass against it, and DESIGN records
the ABI decision (runtime in C versus lowered) with its rationale. The `e2e`
acceptance test moved to W9.2 when the item was split. W9.1's acceptance became
the fuzz corpus, which is what finished the prototype.

**Done.** `crates/cgen/src/clif.rs` lowers the fuzz corpus fragment to Cranelift IR
and emits one object per module through `cranelift-object`. `obc --backend=c|clif`
(default `c`) writes that object and links it with the same runtime and loader.
`obgen --backend=clif` generates the fragment, and `fuzz/run.sh` runs the standard
oracle against it (`BACKEND=clif bash fuzz/run.sh`: **67 programs, 0 failures**).
The regression list has nine seeds for values and traps in the lower-case
dialect, plus 30 coverage seeds and 10 adversarial coverage seeds. DESIGN
section 16 records the research and prototype. The runtime checks are
`static inline` helpers with no link-time identity, so four `ob_rt_*` entry
points had to be added. Runs establish the shift rule (29 of 30 coverage
programs use counts of 32 or more; 6 fail if the rule is dropped), the
`OptLevel`, and the module and command-table shape. They also expose a
discrepancy between the two checks.
A scalar `CASE` with no matching label takes no branch. A type-`CASE` traps and
warns. The report is silent on scalar `CASE`, and the second back end now agrees
with the C path. W2.5 records that decision.

**Split into two items.** Research is done. W9.1 now covers a prototype for the
**fuzz corpus**: `INTEGER` arithmetic and shifts, `IF`/`WHILE`/`REPEAT`/`FOR`/
`CASE`, procedures, recursion, and the runtime calls emitted code makes
(`rt_div_at`, `rt_mod_at`, `rt_trap_pos`, `rt_index`). The corpus is a complete
milestone. Programs have expected values, and they don't use records, arrays,
pointers, descriptors, open arrays or type-`CASE`. Passing establishes the module
and object plumbing, the C ABI boundary for runtime calls, `OptLevel`, and
Cranelift's shift rule. That rule differs from the report's and the corpus tests
it with deliberate large-count shifts.

**W9.4 inlining policy and pass: done**
The pass changed program output on its first run. The cause is recorded below.
Why: `bench/CallArray.mod` calls a procedure once per iteration, and this back
end emits the call. `cranelift-codegen` supplies the inlining machinery.
Its `inline.rs` calls itself "inlining as a library". Compilation is
per-function, so it "does not know which functions are hot and which are cold".
An embedder chooses the policy through an `Inline` trait.
`Context::inline` is the entry point.

Using it here requires keeping every procedure's IR. This back end lowers each
procedure into its own `Context` and defines it, so no callee body survives.
The pass needs a policy and must run before finalising. Start with small,
non-recursive call sites one deep.
Risks to state before anyone writes it: recursion needs a width or depth limit.
Inlining preserves `VAR` semantics because the address is still the caller's, but it
changes code size. A bad policy costs more than it saves. The payoff is bounded:
inlining accounts for one of three causes of the ~10x gap on this benchmark.
Neither vectorisation nor the check lowering (W9.3) is affected.
This back end is worst at a call per iteration: `bench/run.sh` puts `CallArray` at
~10x the C time. Inlining is the one inter-procedural pass Cranelift has. It has no
loop optimisation or auto-vectoriser (see W9.5's sibling note in DESIGN section 16).
Steps, in order: lower every procedure into a `Context` it keeps; add `Inline`
(small, non-recursive, one call deep to start); run `Context::inline` on each before
defining it.
**The first run exposed an output bug.** `bench/run.sh`'s call-heavy case improved
from 9.96x to 5.97x the C time without checks and from ~21x to ~12x with them,
a 4x speedup. The C-mode suite passed. Under `BACKEND=clif`,
`driver-built-program-runs` failed. `Hello.mod` writes "Hello, world!" from its
module body and "Hello again from a command" from `Greet`, but the run printed
the first string twice.

The IR showed the cause. The spliced `WriteString` call has `iconst 27` (the
callee's text length) but refers to the caller's data. A `GlobalValue` reference
does not survive splicing as this back end passed it. Inspection ruled out the
`FuncRef`-to-`FuncId` mapping (the body's own name is `namespace 0, index 17`),
the map contents, the string data (`declare_string` gives each literal its own
symbol and `DataId`), and a stale build. Cranelift's contract requires a callee
that is legalized and valid, and behaves correctly. The clone meets
that contract. The embedder must prepare the callee's entities for
the inliner, so the inliner stays off until that preparation is understood. A
4x benchmark win is not worth a wrong string.

**Fixed.** `Context::inline` copies a callee's `GlobalValueData` verbatim,
including a `Symbol`'s `User` name, but translates a `FuncRef`'s name. A `User`
name indexes the function's own name table, so it has the same meaning in
another function only when both declare the same data in the same order. This
back end declared only strings referenced in each body. An inlined literal
resolved to whatever the caller had declared at that index, making
`Greet` print the module body's string. The fix declares module strings once
and gives every function `GlobalValue`s for all of them in one order. `Bis4`
(`/tmp` scratch, ten lines: a callee assigning a string literal, checked by one
character) isolated the issue. `e2e`'s `driver-built-program-runs` and
`driver-output` under `BACKEND=clif` catch regressions.

Each function now declares module data uniformly, with the same objects in the
same order, or not at all. Both details matter: strings previously formed a
body-dependent *set*, and five other declaration loops (`vars`, `rt`, `msgs`,
`descs`, `root_maps`) walked `HashMap`s whose order differs between functions
and runs. `e2e` covers the data with `InlStr.mod` (the literal), `InlDesc.mod`
(a descriptor the caller never names) and `InlVar.mod` (the order hazard, where
the sets agreed but their sequence differed). Each has its own assertion.

Done when: these rows pass with the inliner on and `bench/run.sh`'s call-heavy
ratio stays at the improved level. **Met**: `e2e` 138/0 in both modes. The
call-heavy ratio changed from 9.96x to 6.51x without checks and from ~21x to
11.95x with them. The corpus also passes.

**W9.7 the optimisation level is the driver's to choose**: **done**
Why: the back end hard-coded Cranelift's `opt_level = "speed"`. This matches the C
path's `gcc -O2`. Cranelift defaults to `none`.
Users could not choose `none` for fast syntax checks that still link, or
`speed_and_size` when that trade-off matters.
`obc ... -Oopt=LEVEL` now accepts `none`, `speed` (the default) or `speed_and_size`,
using Cranelift's names. It refuses other names. The option affects only the second
back end. The C path's level is `gcc`'s and is chosen with `--cflag -O...`.
Done when: `e2e`'s `opt-level-*` rows build and run `Hello.mod` at each level under
`--backend=clif` on both back-end runs, and a name that is not a level fails the build.

**W9.6 the alias analysis: no embedder work, recorded so it is not re-derived**
This Cranelift feature sounds like W9.4 and W9.5, which require an embedder
action. In `context.rs`, `Context::compile` builds
`AliasAnalysis::new(&self.func, &self.domtree)`. `enable_alias_analysis` defaults
to `true`, and this back end's `flags` block leaves it unchanged (the block sets
`opt_level = "speed"` only). A grep checks the item. You do not need to configure
it.
A future `settings` change could turn alias analysis off silently.
`grep -n 'flags.set' crates/clif/src/clif.rs` shows where to check.
Done when: n/a. This records the measurement and the `settings` block it points at.

**W9.5 unwind information**: **open, measured: the objects have none**
`unwind_info = true` is the default, but the embedder must emit the data.
Cranelift's `Context::create_unwind_info` is deprecated in favour of
`CompiledCode::create_unwind_info`, and `cranelift-object` does not call it.
On one module, the object built by `cc` has `.eh_frame` and `.rela.eh_frame`;
the object from this back end has neither. No procedure here unwinds, so a gdb
backtrace, `perf`, or a C++ exception passing through stops at its frame. This
limits diagnostics, not program correctness, so it sits below W9.4.
Done when: an object from this back end emits unwind information and
`readelf -S` shows it, with the suites unmoved.

**W9.3 the second back end's runtime checks are calls, not branches**: **open, found
by `bench/run.sh` (DESIGN section 16)**
On `bench/CallArray.mod`, the second back end ran at ~10x the C back end's time
without checks and ~22x with them when this was written. The C back end emits an
inline compare against the bound and jumps to a cold trap on failure. The second
back end calls `rt_index`/`rt_rt` per access. The same code-shape difference
covers `NIL`, the guard and type-`CASE`. Semantics stay the same. Only the cost
changes. Inlining (W9.4) later moved both figures to ~6.5x and ~12x. The remaining
gap is ~1.8x, down from ~2.2x.
Done when: `bench/run.sh`'s checked-to-unchecked gap on `CallArray` is much
smaller than ~1.8x, with the corpus and every suite unchanged.

**W9.2 the rest of the language on the Cranelift back end**: **done**
(a-d) are in. `BACKEND=clif bash e2e/run.sh` reports **131 passed, 0 failed**,
matching the C path's rows. Both back ends pass the corpus. DESIGN section 16
records the work. The final failure came from a frame-held record local that
missed its descriptor walk, so its type-`CASE` read `NULL + 8`.

**Mixed-link results.** Qualified calls worked only in *statement* position.
The back end refused `Lib.Twice(21)` in a *value* position, so it could not use
results from another module. `call_value` now has the branch already present in
`call_stmt`. The rows use two modules per back end for qualified calls, plus one
program linked through `cc` in both directions.

*Notes from W9.1 for the next session:*

* The W9.1 lowerer's refusals define the work list. Replace each refusal at its
  site (`scalar`/`data_size` for a type, the `Selector` arms of
  `designator_value`/`assign` for one, and `call_stmt` for a predeclared name).
  Each refusal names W9.2 and the byte where it occurred. The back end can then
  add one construct at a time without lowering unsupported constructs.
* **The layout follows the C ABI** (DESIGN section 16's layout paragraph is
  corrected). `cgen` emits C structs and `cc` lays out their members. The second
  back end must reproduce that layout and check it against `cc`'s emitted-header
  report. Build this first because records, arrays, pointers, `NEW` and the
  collector's field maps depend on it.
* Data objects use relocations. `oberon.h` defines the `ob_rt_*` object-boundary
  entry points. The driver's loader calls the `__init`/`__load`/command-table
  model.
* `obgen --backend=clif` renders the scalar fragment. Its two collector blocks
  are C-only. After records and pointers are supported, enable them in
  `fuzz/run.sh`'s clif mode and widen the adversarial variants. This gives the
  pointer graph an oracle.

W9.1's prototype excludes records and their descriptors, `ARRAY` and open-array
parameters, `POINTER TO` and `NEW`, type guards and type-`CASE`, `SYSTEM`,
`REAL`/`CHAR`/`SET`, and string and record copies. The constraints are fixed:
the descriptor is at offset 0, the runtime uses the C ABI, and layout follows the
C compiler (see the correction above). The remaining work is to emit the same
shapes through another IR. `e2e` is the acceptance test the prototype did not
yet meet.

**The work was split into slices so each part could be completed and tested.**

* **W9.2a, layout and arrays.** The layout module computes sizes, offsets and
  alignment using the C ABI for the shapes we emit: basic types, fixed arrays,
  and records whose base is their first member and whose descriptor is at offset
  0. A test makes `cc` the authority by compiling the emitted header and
  comparing `sizeof`/`offsetof` with the computed table. Scalar fixed arrays
  follow that test. **Done**: `crates/cgen/src/layout.rs` computes the layout
  and `obc emit --layout` prints it. This diagnostic stops before emitting, so a
  module with records the back end cannot yet lower can still report its layout.

  `e2e`'s layout block compares 20 facts, one `check` per fact. The cases include
  a record with `INTEGER`/`CHAR`/`REAL`/pointer fields, an extension, arrays of
  records and pointers, a trailing `CHAR`, and `RECORD END`. Every fact agrees
  with `sizeof`/`offsetof` from `cc` on the C back end's header. The comparison
  detects alignment errors: using 4-byte pointer alignment instead of the
  target's produces 7 failures, each naming a type and field (`layout-Cell.next`,
  `layout-Paired.extra`, `layout-Nodes`, ...). Correcting the alignment clears
  those failures.

  Scalar fixed arrays use this layout, including multidimensional and local
  arrays. A local array is a zeroed frame slot, as the C back end's `memset`
  makes it. Each dimension uses one `rt_index` check against its length and the
  designator's byte. An out-of-range second subscript reports
  `index out of range (at byte 54 ...)` on both back ends. `e2e` reports 122
  passed and 0 failed, up from 101. The scalar corpus passes on both paths.

  A harness issue appears before W9.2c's rows. With `BACKEND=clif`, the suite
  stops around row 69 at `checks-off-emits-no-check`. That row greps emitted
  `Checks.c`, which a back end that emits an object does not write. Under
  `set -e`, the grep ends the run. Rows that inspect emitted *C* must specify
  `--backend=c` instead of inheriting `BACKEND`. This is a harness issue.
* **W9.2b, records, pointers and the heap.** This slice covers descriptor
  emission matching the C emitter's `ObDesc` and collector field map, record
  locals, fields, assignments and copies, `POINTER TO`, `NEW` through `rt_new`,
  and the type-`CASE` and guards that read a descriptor. Its acceptance check
  runs the corpus's *collector* blocks in clif mode (`fuzz/run.sh`,
  `obgen --backend=clif`).

  **Done**: the clif path runs the C path's seven blocks, with 86 programs and
  0 failures, including the collector stress blocks. `BACKEND=clif bash
  e2e/run.sh` reaches 56 check lines. W9.2c and W9.2d address the remaining gap.
  Descriptors, field maps and root registrations are data objects under the C
  back end's names, with offsets from `layout.rs`. `NEW` allocates through
  `rt_new` and initializes nested descriptors. Fields, implicit dereferences,
  `NIL` checks, record copies (`rt_rec_copy`), `IS`, guards and type-`CASE` read
  descriptors through `ob_rt_is`.

  The `obgen` fragment mode and `run.sh`'s clif-only branch were removed, so both
  back ends use one corpus. `BACKEND=clif bash fuzz/run.sh` runs the same seven
  blocks and reports **86 programs, 0 failures, exit 0**, including two collector
  stress blocks with the collector forced on every allocation. The C path exits
  0 unchanged. UBSan found a module pointer variable aligned to a word instead
  of its type, causing a misaligned load during the collector walk on four
  regression seeds. The C back end's `IS` and type-`CASE` also read through a
  `NIL` pointer where the report says "no match"; the lowered code returns that
  result. Clif `e2e` reaches 56 checks, with 9 failing. W9.2c addresses the
  remaining work.
* **W9.2c, value types, open arrays and `VAR` parameters.** This slice covers
  `REAL`, `CHAR`, `SET` (including the ordinal and set operations that `csmoke`
  tests on the C side), open arrays with hidden lengths, `VAR` parameters, and
  string and record copies required by parameter modes. Acceptance requires
  `BACKEND=clif` to pass the `e2e` rows for values and parameters.

  **Done.** `REAL` uses `f32` and float instructions (`fcmp` for comparisons,
  `FLT`/`FLOOR`/`ABS`, with `FLOOR` saturating where C's cast is undefined).
  `CHAR`/`BYTE` uses one unsigned byte for comparisons and conversions
  (`ORD`/`CHR`, ordinal order, and `rt_charcmp` for character arrays). `SET` uses
  a `uint32_t` mask (`bor`/`band`/`bxor`/complement, `ob_rt_set` for a constructor,
  and the `& 31` bit test for `IN`). A `VAR` actual uses the caller's address.
  The caller copies a structured *value* actual. Open arrays pass a pointer and
  hidden length for the index check. String literals use data objects:
  `rt_strcopy` copies them into arrays, and array-to-array copies use bytes.
  Cross-module calls and imported variables were also added for `Hello`/`Out`.

  **`BACKEND=clif bash e2e/run.sh`: 66 checks pass, 3 fail** (from 56/9 at
  the slice start; C-path `e2e` is 126/0). The failing rows include
  `Torture.mod`, which declares a **procedure variable**, and `GcRoots.mod`,
  which needs a descriptor in *another* module's object. A mixed build must
  import an imported record type's `TD_` rather than emit it locally. The
  failures are refused by name with their byte. The corpus passes on both back
  ends (86 programs, exit 0).

  *Found here*: DESIGN section 9.4's table names `rt_charcopy` for an
  array-to-array `CHAR` copy, but the header has no such function. The C back
  end uses a C struct assignment for a full-length copy, so the lowerer copies
  bytes itself. The C-side table did not need to justify this row.

* **W9.2d, scope machinery and the remaining rows.** This slice covers nested
  procedures with static links, procedure variables, `SYSTEM`, imported record
  descriptors and the remaining `e2e` rows. Done when
  `BACKEND=clif bash e2e/run.sh` passes the same 126 rows as the C path.

  **Done.** `SYSTEM` is implemented for all six entries in section 9.10's
  address model. The IR makes the C's implicit conversions explicit, such as
  comparing an `intptr_t` address with an `INTEGER`. Calls through procedure-
  valued record fields also work. The last failure was a local record in a
  frame: the walk that initializes each local record's descriptor scanned only
  slot locals. It skipped records in a frame. ASan reported
  `Regress__localRecord__which` and `SEGV on address 0x8`. The walk was fixed.
  **`BACKEND=clif bash e2e/run.sh`: 126 pass, 0 fail**. The C path is 126/0,
  and the corpus passes on both back ends.

  *Superseded note.* An earlier run implemented nested procedures with static
  links: one frame per nesting procedure, a hidden link parameter, link walking
  for enclosing locals, links passed at nested calls and symbols keyed by
  lexical path. It also added `PACK`/`UNPK`, a `CASE` over `CHAR`/`BYTE` and one
  block per `WHILE` guard. **`BACKEND=clif bash e2e/run.sh`: 119 pass, 7 fail**.
  The earlier 67 was when the harness stopped early. That row now reports.
  Remaining work then included a call through a procedure-valued *field*
  (`Regress.mod`, refused by name) and **`SYSTEM`** (four rows:
  `ADR`/`SIZE`/`BIT`/`GET`/`PUT`/`COPY`, the builtin import's interface line, and
  the search-path shadowing pair), plus one cross-module anonymous-type name.
  The gate and corpus passed on both back ends.

  *Superseded note.* The next run implemented two items. An interface's record
  type now *imports* its `TD_` rather than defining one, so `IS`, a guard and a
  type-`CASE` agree across a module boundary
  (`gc-roots-hold-their-objects` verifies this). The layout walk follows a
  pointer to its base and records a pointer's layout before recursing. Otherwise
  `P = POINTER TO Node; Node = RECORD next: P END` does not finish. The two
  `Gc.mod` rows caught this. Procedure values are code addresses (`func_addr`
  for a module-level procedure, `call_indirect` for a call, with the signature
  recorded by the checker). This supports `Torture.mod`'s
  `fn := twice; fn(21)`. **`BACKEND=clif bash e2e/run.sh`: 67 pass, 2 fail**
  (from 66/3). The remaining rows required **nested procedures with static
  links** and **`SYSTEM`**. `Torture.mod` declares `PROCEDURE inner` and reaches
  `SYSTEM`. Both were then refused by name. The gate and corpus passed on both
  back ends.

**W7.7 a byte for every trap, not only the memory ones (done)**

The matrix asserted an exact source offset for index and guard traps, because those
messages carry one. `DIV by zero`, `MOD by zero`, `assertion failed` and
`CASE: no matching type` carried no offset. Those errors gave users no location,
although the runtime held the offset at each site.
**Done**: `rt_div_checked`/`rt_mod_checked`/`rt_guard_at` take the expression's
offset and the `CASE` default calls `rt_trap_pos`, all four messages now read
`(at byte N of the source)`, and the matrix's four rows assert the exact line
(`DIV by zero (at byte 55 ...)`, `MOD by zero (at byte 55 ...)`, `assertion failed
(at byte 19 ...)`, `CASE: no matching type (at byte 137 ...)`). The two-argument
`rt_div`/`rt_mod`/`rt_guard` remain as wrappers, because `golden`'s hand-written
fixtures call them directly and have no offset to pass.

## W10: a garbage collector

**W10.1 the collector: the decision, and what it costs the emitter** (done)
(the design is DESIGN section 15. The code is in W10.2.)

Oberon-07 provides `NEW` but no `DISPOSE`. The report's section 6.4 defines allocation
and says nothing about freeing, so every program this compiler builds leaks by
construction. A long-running one dies of it rather than of its own logic. No
amount of book-keeping in the front end fixes that. This is a runtime subsystem.
The design makes collection cheap here: there is exactly one allocation point
(`rt_new`, made the only one in W7.4), and each heap object begins with its own
`ObDesc`, giving the collector a place to hook and something to walk.

Research to start from. Read it, do not assume it: the Oberon report section 6.4 (`NEW`
and the absence of a free); this design's descriptor, runtime and frame sections
(6.4, 9.2, 10); the chapters on runtime systems and memory management in
`books/` (Appel, *Modern Compiler Implementation in C*; the *Compiler Design
Handbook*); and Wirth's own Oberon compilers, whose `NEW`-plus-descriptor scheme is
the ancestor of this layout. Decide and record, with the reason:

1. **Precise or conservative marking.** A precise mark needs a pointer map per
   type, which the descriptor does not carry today (it holds base, size and a
   name). A conservative mark needs neither a map nor a stack map. It must account
   for values in frames that are not pointers, such as the hidden open-array length,
   an `int32_t` that may look like one, and for interior pointers. Retaining garbage
   is acceptable. Freeing a live object is not. The C back end cannot supply a stack
   map, so conservative is the only option there. The Cranelift back end can (W9's
   research), supporting a descriptor-driven mark with a conservative stack and
   register walk first, then precise stack maps as its follow-up.
2. **What the descriptor gains.** If the mark is precise, the descriptor needs the
   pointer-field information the emitter can already produce. `desc_init` walks
   exactly those fields to install nested descriptors. The item must say what
   that costs in object size and when the interface format version changes (W3.2).
3. **The root set, stated explicitly.** Module-level variables, the live state of
   the command table, and the stack. A value parameter's *copy* is a root, a `VAR`
   parameter's original is, an open array's hidden length is not, and a `NIL` is not.
4. **When to collect.** Before `NEW` gives up, so that the report's
   `NEW`-yields-`NIL` (section 6.4) keeps its meaning with a bounded heap. This keeps
   `OB_ALLOC_FAIL_AT` a *test* hook rather than the only path to a failing
   allocation. A collection that frees nothing must stop, not loop.
5. **What it must never do.** Free a live object (the fuzzer's self-checks are the
   oracle), or move objects. The emitter addresses them through raw pointers
   today, so this collector reclaims without compacting.

Plan: extend the descriptor with whatever the chosen design needs. Put the collector
in the runtime header (a mark stack, a sweep over the allocated list, the `rt_new`
hook), keep the allocator's single entry point, and drive it from a mode the tests
can force: `OB_GC_STRESS=1`, following the shape of `OB_ALLOC_FAIL_AT`.

Done when: a program that allocates in a loop runs in a bounded heap; the fuzzer's
seeds pass with the collector forced on every allocation; and the design states the
root set and the mark/precision decision, with the reason it was taken. **The
decision half is done and recorded as DESIGN section 15**, decided from Wirth's 14.2 (the
three facts a collector needs, and the descriptor that holds two of them),
Mogensen's 12.2 (the root set, so a conservative stack walk is needed where there
are no stack maps) and the *Compiler Design Handbook*'s chapter 6 (the method
taxonomy, which rules out moving collectors for a back end that addresses objects
through raw pointers): mark-sweep, a precise mark driven by a descriptor that gains
the pointer-field offsets, a conservative stack walk now and a precise one when W9's
back end can supply stack maps, and collection before `NEW` gives up.
The code is in W10.2.

**W10.2 the collector itself** (implemented: `crates/cgen/src/oberon_h.inc`,
`crates/cgen/src/oberon_runtime.inc`, `crates/cgen/src/cgen.rs`, `crates/obc/src/bin/obgen.rs`, `fuzz/run.sh`,
`e2e/run.sh` (five rows) and one line of `e2e/Texts.c`. Its hand-written descriptor
literal had to list the descriptor's two new fields like any other positional
initializer under the e2e build's `-Werror`**

Use DESIGN section 15. Extend `ObDesc` with the pointer-field offsets the emitter
already knows from `desc_init`'s walk. The runtime needs a mark stack, a sweep over
the allocation list and the `rt_new` hook. Register each module's pointer variables
as roots, and add `OB_GC_STRESS=1` so tests can force collection before every allocation.
Settle the three open questions listed in section 15. Choose the allocation-list
shape (a header before the payload, or a table), define the trigger besides a failing
allocation, and specify how stress mode avoids changing a program's answers.

Done when: a program that allocates in a loop runs in a bounded heap; `fuzz` passes
with the stress mode on (its self-checks are the "no live object was freed" oracle);
and the standard suite is green with it off, so the collector costs nothing when it
is idle.

**Done**, and section 15 now records each answer with the run that executes it:

* **The list is a header in front of the payload** (`ObBlock`: next, payload size,
  mark), so the descriptor stays at offset 0 of the object and the sweep has both the
  next block and the size an interior pointer is tested against. The descriptor gains
  `const ObFld *fields; int32_t nfields;`, where an entry is
  `{off, stride, n, desc}`: pointer slots (`desc == NULL`, one or a run), or a run of
  records whose own map takes over. A type without pointer fields has a NULL map, so
  marking it is one NULL test. The map is emitted by the same walk `desc_init` uses,
  with `offsetof` expressions, so it follows the C layout the back end does not
  compute for itself.
* **The trigger besides a failing allocation is `OB_GC_LIMIT=n`**, a byte budget on
  headers plus payloads: over it, the collector runs, and `NEW` yields `NIL` if it is
  still over. This is the report's failure case, with a heap that never runs out of
  malloc. A collection that frees nothing ends the attempt. It does not loop.
* **`OB_GC_STRESS=1` collects before every allocation** and does not change answers:
  only unreachable blocks are freed, nothing moves, and which address a later `NEW`
  gets was always `malloc`'s to decide. Roots are each module's own variables,
  registered from `__init` before its body can allocate (a pointer variable's slot,
  a record variable's type, an `ARRAY OF RECORD` as a run). The command table
  doesn't need an entry because it holds names and code addresses only. Everything
  else is the conservative stack walk, using `setjmp` for registers in a frame.
  The runtime also collects once at exit, so a leak checker sees what the program
  still holds.
Evidence: `e2e`'s six collector rows (1000 rounds of 20 nodes = 20000 allocations in
a 4096-byte heap, completing in stress, limit and idle modes with the same
checked value; a live chain over the limit reaching `NIL` and counted back intact;
and one root of each kind, including a type whose map lives in another module and
one whose pointer field the base of the object's type declares); the generated
fragment's heap probe, which keeps a chain per root kind across churn; and `fuzz`'s
two stress blocks, built with the sanitizers whether or not `SAN=1` is set. A freed
object whose memory is not reused still reads correctly, so ASan caught the defect
while the walk was being written (under `-fsanitize=address` locals live in a fake
stack the walk cannot see, so the runtime turns `detect_stack_use_after_return` off
and unpoisons the region it reads). The standard suites are unchanged with the
collector idle: no stress or limit, and the only path that collects is a `malloc`
failure.

**W10.3 collector pauses: measured, with the bound declined for a reason**
(`OB_GC_STATS=1` and `fuzz/gc-bench.mod` are the instrument and the benchmark; numbers
and the path they point at are at the end of this item)

The collector in section 15 is stop-the-world, so a pause is proportional to the
live set *plus* the whole heap: marking walks the reachable graph and conservatively
scans the stack, and sweeping walks the allocation list whether an object is live or
not. The trigger is a *failing* allocation, exactly when the heap is fullest. Deep
recursion also lengthens the stack scan in proportion to its depth. For the scale
this compiler targets, the design accepts that trade-off. Users will meet the
property, so it deserves its own item rather than a footnote.

What to reach for, in order of cost, each with the source that describes it:

1. **Trigger on a threshold**, not only on failure: the same total work spread over
   time, and it removes the grow-to-the-limit-then-pause shape. It needs the heap's
   high-water mark per program, which is the same instrument as (4).
2. **Incremental marking**, described in the *Compiler Design Handbook*'s chapter 6,
   section 6.5 ("Incremental and Concurrent Non-Moving Collectors"). A non-moving
   collector lets a mutator keep running while the mark advances, with a barrier at
   the points where it hands a pointer over.
3. **Concurrent marking**, then **generational** (section 6.4) if allocation rates
   say so. Generational cuts pause times most, at the cost of write barriers plus
   intergenerational-pointer bookkeeping. This changes the *emitter* as well as the
   runtime. Every store to a pointer field would need the barrier.
4. **Shorter conservative scans** before any of that: an instrument that reports the
   pause distribution (`OB_GC_STATS=1`, in the same style as `OB_ALLOC_FAIL_AT` and
   `OB_GC_STRESS`), so the choice is made from measurements rather than the argument
   above. The Cranelift back end's stack maps (W9) are the real fix for the stack
   half: they make the scan precise and bound it by the *live* frames rather than
   by depth.

Done when: a benchmark's pause distribution is measured and recorded (worst and
median, with the heap it ran in). A stated bound is met or explicitly declined with
the reason. The correctness oracle is unchanged: `fuzz` with `OB_GC_STRESS=1` and
the standard suite must both still be green. "We measured, and this is the bound we
accept" is a legitimate outcome. "It seems fine" is not.

**First step is the instrument, and it is required**: the collector in section 15 has
no counter of any kind, so wall-clock numbers cannot separate a pause from the mutator
work between pauses. `OB_GC_STATS=1` reports, per collection, the bytes before and
after, the blocks marked and swept, and the *time spent inside* `ob_collect`.
Everything needed is already in the runtime. The switch goes beside the other two in
`ob_gc_env`; `ob_heap_bytes` supplies byte counts before and after; the mark stack
shows what was marked. The sweep loop distinguishes kept blocks from freed blocks,
and the clock is `timespec_get(&ts, TIME_UTC)` (C11, so no feature-test macro and no
new dependency). The setting follows the shape of `OB_ALLOC_FAIL_AT` and
`OB_GC_STRESS`. It writes to *stderr*, so a program's own output is untouched.
A final line at exit gives the totals. A benchmark then reports a distribution
instead of an impression.

**The instrument, and what it measured.** `OB_GC_STATS=1` reports per collection and
totals at exit (bytes before and after, blocks kept and freed, time inside
`ob_collect`), on stderr, off by default. The benchmark is `fuzz/gc-bench.mod`: 5 000
live nodes kept while 200 000 objects are allocated and dropped, so the collector has
something to reclaim and something to keep. Built with the documented line and run as
`OB_GC_STATS=1 OB_GC_LIMIT=<bytes> ./gc-bench`:

| heap ceiling | collections | worst pause | median pause | total inside the collector |
|---|---|---|---|---|
| 2 MB | 6 | **1.03 s** | 0.75 s | 4.26 s of a ~8 s run |
| 512 KB | 36 | **0.28 s** | 0.11 s | 4.14 s of a ~8 s run |

Those are one run's numbers, and repeat runs on this machine vary by about 20% (the
same benchmark, same ceiling, worst pause 0.33 s at 36 collections), so they are read
as a shape rather than a limit: the worst pause tracks the heap the collector is given,
roughly linearly, and a run's total collector time does not move with it.

**The bound is declined, with the reason.** No interactive bound survives those
numbers: 0.28 s is four hundred times a 60 Hz frame. The benchmark's *shape* explains
the 1.03 s pause at the larger ceiling: 200 000 objects become garbage before the
first collection, so the first sweep walks the whole heap. This is not a steady state.
Total time inside the collector, ~4.2 s either way, matters more. It shifts between
few long pauses and many short ones as the ceiling shrinks. The measurement shows the
work is unchanged and this design does it with the mutator stopped.

So the path is the one in section 15 and this item list, in order: **trigger on a
threshold** rather than only on a failing allocation. It removes the
grow-to-the-limit burst and spreads the same work. **Incremental marking** is
reachable only because this collector does not move objects. Next comes concurrent
marking, then generational collection if allocation rates ask for it. The numbers
above are the baseline those changes have to beat. The ceiling, the knob available
today, is recorded with them.

**One early observation, with its caveat.** A churn benchmark of my own, with 5000
live nodes kept while 200 000 objects are allocated and dropped, ran
*unbounded* in 12.0 s and 12.8 s, at a 2 MB ceiling in 5.7 s and 6.8 s, and at
512 KB in 10.6 s and 7.6 s. The bounded runs being *faster* than the unbounded one is
not the collector being free. Reuse beats growth: the unbounded run grows the heap
to ~6 MB and pays for the pages, while the bounded one stays cache-hot.
This collector trades memory for pauses rather than speed for safety. The honest
caveat is that those numbers came from a machine compiling concurrently, so they
offer an early indication. The instrument is what turns them into a measurement.

## Ideas not yet in the design (adopt or drop)

- Fuzz the *lexer* with random bytes and random token streams: assert no panic
  and that every diagnostic is located. W6 covers whole programs; this would
  attack the input that has no structure at all.
- Grow the corpus where it is thin: `e2e` has the report's modules, but the
  *diagnostic* catalogue (section 8.5's 55 rows) is exercised only by unit tests; a few
  modules per row would test the driver path too.

**W11.1 the host's arguments, as a module rather than a built-in** (done)
Why: the report has no notion of a command line. A module is activated, not
invoked, and a real program needs its arguments. The tempting shape is a built-in
name, which is what `oberonc`'s `ARGNUM`/`ARGS` are and why its own examples build
there and nowhere else. Here, the module's body is in the runtime (DESIGN section
17). `IMPORT Args` is an ordinary import, and both back ends already know how to
call it. Each new host-provided feature costs a row in `RUNTIME_MODULES`, its C and
its `.sym`. No checker arm, back-end arm, or predeclared name is needed.
Done when: a program written for another Oberon compiler builds and runs here with
its argument access ported and nothing else changed. **Met**: `e2e`'s
`third-party-program-*` rows build and run `PrimeNumbers.mod` and `Powers.mod` from
`lboasso/oberonc` (MIT), with two lines of port each and `Util.mod` unmodified, on
both back ends. The suite has 148 rows in either mode.

## Non-goals (deliberate, DESIGN section 1)

Optimisation of any kind. The C compiler does it. Oberon-2 and later dialects,
release packaging and publishing are also out of scope. Nothing under `../books/`
is versioned. Those PDFs stay on disk and ignored.
