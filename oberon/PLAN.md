# Oberon-07 → C: implementation backlog

This is the open work, derived from `DESIGN.md` itself rather than from
enthusiasm: every item names the section or risk it comes from, the files it
touches, and what makes it done. Paths are relative to this directory.

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

`bash fuzz/run.sh` joins it (W6.2): by default it runs `fuzz/regressions.txt` —
every seed that once found a defect, each with the reason it is listed, because a
seed reproduces a program exactly on any machine — and then a 100-seed coverage
block. `SEEDS`/`BASE` select one contiguous range instead, for an ad-hoc run;
`COVER` sets the block's size. It reported five seeds failing when it was first
written and they are all fixed, which is the point of keeping the list.

Recently closed, for context: DIAG's located diagnostics and statement-level
recovery (DESIGN §5 D7, §10), the CLI's warning path (a clean module used to
hide its warnings), this document's move into the crate, and the random program
generator itself (W6.1).

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

## W0 — make the document's own claims true

**W0.1 clippy is not clean** — **done**
Why: the preamble says "clippy clean"; `cargo clippy --all-targets` reports 244
warnings, *all* in LALRPOP's generated parser
(`target/…/out/oberon.rs`, `clippy::ptr_arg` — `&mut Vec` is what LALRPOP's
`ErrorRecovery` signature requires, so it cannot be changed).
Files: `crates/syntax/src/lib.rs` (the `lalrpop_mod!` site).
Done when: an `#[allow(clippy::ptr_arg)]` — scoped to the generated module, not
the crate — makes `cargo clippy --all-targets -- -D warnings` exit 0, and the
clippy line is part of the gate above. **Done**: `lalrpop_mod!` carries the
attribute (LALRPOP's recovery hook is `&mut Vec<..>` by its own API, which is why
`ptr_arg` fires 244 times inside the generated file and nowhere in ours), and the
gate block below no longer carries a caveat.

**W0.2 two sections are still marked `[design]` although they are executed daily** — **done**
Why: §9.2 (type mapping) and §9.3 (records, extension, descriptors) are marked
`[design]`, but `crates/cgen/src/cgen.rs` implements them and `e2e`/`csmoke` run them.
Done when: those markers say what is actually unverified (if anything), so that
`grep '\[design' DESIGN.md` returns only the legend. **Done**: §9.2 and §9.3 are
now `[verified]` with the suites that execute them named — the document's own
legend defines `[design]` as "not yet implemented", and every row of the mapping
table and every rule of the descriptor scheme is exercised by `csmoke`, `e2e` or
`golden`. `grep '\[design' DESIGN.md` now returns the legend and nothing else.

---

## W1 — the rules the checker still does not enforce

These are DESIGN §8.5's own list of four unchecked things, quoted verbatim.

**W1.1 field visibility across modules** (rows 4 and 14's private-field halves,
report §6.3) — **done**
Why: a field's `*` is accepted, but *who may read it* is not modelled: it is an
interface-layer rule this pass does not have the data for. Three things are
missing, and they are a chain:
1. `types::Type::Record { base, fields: Vec<(String, TypeId)> }` has no
   per-field export mark;
2. `cgen::render_type` renders a record as `RECORD (Base)` — **without its
   fields** — so an interface cannot carry them, which is why the `Texts.sym`
   fixture (hand-written, per `e2e/run.sh`'s header) carries a field list the
   emitter would never produce;
3. `sema` therefore cannot compare the accessing module with the defining one.
Files: `crates/sema/src/types.rs`, `crates/cgen/src/cgen.rs`, `crates/sema/src/sym.rs`, `crates/sema/src/sema.rs`.
Done when: the mark travels through the interface, an access to a private field
of an imported record is an error, and `check_tests` has a case per row (each
case names its row number). **Done**: `types::Field` carries the mark — the
reconnaissance in this item was wrong about the prerequisite, since the writer
already emitted field lists and the reader already parsed them, so the work was
the *mark*, not the format; `interface_env` marks every type an interface
declares, so the field selector can tell the record came from another module; the
writer emits each field's own mark instead of a `*` on all of them; and the
fixtures had to be corrected, because the report's own `Out.WriteLn` reads `W.buf`
from `Texts` — `buf` *is* exported in the report's library and the hand-written
`Texts.sym` said otherwise. Every row of §8.5 is now checked, which is what this
wave was for.

**W1.2 row 50's locality half** (report §6.5, §10.1) — **done**
Why: a *local* procedure assigned to a procedure variable is rejected, but one
passed as an *actual* is not — the check exists on one side of a symmetric rule.
Files: `crates/sema/src/sema.rs`.
Done when: passing a nested procedure where a procedure variable is expected is
an error, with a test. **Done**: `check_actuals` rejects a `local procedure`
actual when the formal's type is a procedure type (the formal is checked first so
a plain type mismatch is not reported twice), which makes the rule hold for both
of the report's halves — `f := Inner` and `Taker(Inner)`. Catalogue row 50 now
has both shapes, and a probe confirms the module-level case still passes. §8.5's
list of unchecked rules is down to two (both halves of field visibility, W1.1).

**W1.3 row 54's unused-*procedure* half** — **done**
Why: unused imports warn; an unused local procedure does not.
Files: `crates/sema/src/sema.rs` (the `unused_imports` pass is the natural home).
Done when: a module with an uncalled local procedure warns once, the warning is
located, and it is visible through `obc check` (the path fixed this session).
**Done**: `Sema::unused_procedures`, with uses recorded at the one place every
*use* of a name passes through (`lookup_qual`; the silent probes call `lookup`
and do not count), identities assigned per declaration so shadowing cannot
confuse it, and the caret on the procedure's *name* — which needed a span in the
AST (`Proc::name_span`, captured by the grammar) rather than arithmetic on the
declaration's span. A call, a use as a value and an exported procedure all count
as used; a self-recursive but otherwise unreachable procedure counts as used,
which the code says is the imprecision. Tested by
`diag_tests::an_uncalled_procedure_warns_and_a_used_one_does_not` (module-level
and nested, plus four negative cases) and DESIGN §8.5's list of unchecked rules
is down to three.

**W1.4 settle §13's open questions and delete them** — **done**
Why: three questions have been open since the design was written, and one is
already answered by the implementation:
- `ARRAY 0 OF T` — **already an error**: `an array length must be positive
  (report 6.2)`; pin it with a test and remove the question;
- a field that hides a base record's field — proposal was "allow with a
  warning"; decide and record;
- unused imports — the design proposes "warning, and off by default", but the
  implementation warns by default. Decide (recommendation: keep warning by
  default, since the suite's clean modules do not import anything unused) and
  make design and code agree.

**Done**: all three are now decisions in DESIGN §13 and the stale "Open
questions" block is gone (it was addressed "when M1 starts", and M1 is finished).
`ARRAY 0 OF T` was already an error and is pinned by catalogue row 41; the
field-hiding rule is now *implemented* rather than only allowed — the checker
warns per field name, with the diagnostic naming why (`diag_tests::
a_field_that_hides_an_inherited_field_warns`); unused imports keep warning by
default, which §13 records.

---

## W2 — known deviations with a real failure mode

**W2.1 R17: the reclassification pass uses module-wide name sets** — **done**
Why: `resolve.rs` splits guard-vs-call and folds module qualification *before*
the checker builds scopes, so a local variable that shadows an import alias, or
a nested type name, is classified by the wrong scope. This is a correctness
bug for programs that shadow, not a style matter.
Files: `crates/sema/src/resolve.rs`, `crates/sema/src/sema.rs` (it already builds the scopes the pass
needs).
Done when: a module whose local shadows an import alias checks correctly (or is
rejected for the right reason), with a test; then §13's R17 becomes a note
about the prototype's pass order history rather than a live limitation. **Done**:
the pass keeps a scope chain — one scope per procedure with its parameters and its
own declarations, and *every* declaration is recorded, not just types and import
aliases, because a variable of some name shadows a type of that name (that was the
actual bug: a local `T` was invisible, so `Q(T)` stayed classified as a guard).
The two programs that used to be rejected now check clean, with the before/after
in §13's R17 and a test pinning both.

**W2.2 R15: a *failing* guard with the record spelling is statically out of bounds** — **done**, and the cause was elsewhere
Why: `p(S).b` where `p` points at `R` reads through a pointer that cannot hold
an `S`; the guard's tag test traps first at run time, but gcc's
`-Warray-bounds`/`-Wmaybe-uninitialized` can see the read statically and reject
the program under `-Werror`. The pointer spelling has no such read, which is why
the e2e trap case uses it.
Candidate fixes: lower the guarded access through a union of the possible
record layouts, or through an opaque `char *` plus `memcpy` (both keep the trap
first); a local pragma is the fallback, documented rather than silent.
Done when: the same program compiles under both spellings with gcc *and* clang
and still traps with the right offset. **Done**: the obstacle was `rt_new` being
`static inline` in the header, which let gcc fold `malloc(sizeof(R))` and prove the
guarded write was outside the allocation; it is defined in `oberon_runtime.c` now,
and the failing record-spelling write compiles and traps with the guard's offset
under both compilers (`trap-guard-record-spelling-as-target` for gcc, and the same
program built with `--cc clang`). The pointer spelling was already tested.

**W2.6 a `NIL` pointer where a type is asked for faults on the C path** — **done**

**Found by W9.2b** (the second back end), by running the same module through both:
`p IS T` and a type-`CASE` on a `NIL` pointer subject read the descriptor at address
zero, so a program asking a *question* died of a fault with no message. The guard
spelling had always checked first, and the lowerer answered the report — `NIL IS T` is
false, and a `NIL` subject matches no label — which is how the C path's gap showed up.

**Fixed** in the same change as the finding: `IS` orders its test as the guard does,
and a pointer type-`CASE` switches on the "no type" sentinel `0`, which no label
carries, leaving the default arm or W2.5's located trap to answer. Row:
`nil-has-no-type`, which fails on the old code by dying.

**W2.5 a scalar `CASE` with no matching label falls through, silently** — **done**

**Found while writing the second back end** (W9.1), which had to choose between
agreeing with the C path and trapping: it agrees with the C path, which is the
reference, and the choice is left here.

What is true, having read the report rather than trusting a summary of it: §9.5
defines which arm runs and that the labels' types must fit the expression, and says
**nothing** about a value that matches no label. So both behaviours are conforming,
and ours are *inconsistent*: a **type**-`CASE` with no match traps with its source
offset and warns when it has no default arm (catalogue row 39 and §9.4), while a
scalar `CASE` with no match and no default falls through without a word. The second
is a silent undefined result where the first is a located diagnostic, which is the
opposite of this design's stance everywhere else — and a program that relies on the
fall-through would stop compiling into the same behaviour, so it is a decision rather
than a fix.

Options, with what each costs: **trap** (consistent with the type-`CASE`, with
"trap rather than silence", and with the design's own treatment of a missing guard;
it changes the emitted C for any scalar `CASE` without a default, which is why it
needs saying out loud); **warn and keep falling through** (the cheapest consistency:
the reader is told, the behaviour is unchanged, and catalogue row 39's note covers
it); or **leave both** and record that the asymmetry is deliberate, which needs a
reason better than "it was always that way".

**Decided: warn and trap, symmetric with the type-`CASE`.** The reason is the one
this design uses everywhere else — a silent undefined result is worse than a located
one — and symmetry settles the rest: a type-`CASE` with no default arm warns, and one
whose value matches no label traps with its source byte, so a scalar `CASE` should do
both. The cost is stated rather than hidden: the emitted C for a scalar `CASE`
*without* a default arm gains a trapping `default` and a warning, so a program that
relied on the fall-through changes behaviour — which is the point, since that
fall-through is exactly the silence being removed, and the design's §9.6 row records
it as a deliberate choice rather than as something that happens to be true.

Done when: DESIGN §9.6's row for the scalar `CASE` says what the choice is rather than
what the code happens to do, the warning and the trap are emitted (the trap with its
byte, the warning only when there is no default arm, as for the type-`CASE`), the C
back end and the Cranelift one agree on it — W9.2b lowers this shape, so the two must
land together or the second inherits the old silence — and the suites pin both: a
`trap-` row in `e2e` asserting the exact line, and a row for the warning. **Done**:
the emitter writes the trapping `default` under `-Ochecks=case`, the checker warns for
any `CASE` without a default arm (catalogue row 39), §9.6's row says so, and the
Cranelift lowerer moved with it in the same change — same message, same byte (the
subject's span), same switch — verified on both back ends. Rows:
`trap-case-no-matching-label` (asserting the exact line and the abort),
`a-default-less-case-warns`, and the fixtures that relied on the old silence updated
rather than accommodated: `Regress.mod`'s ranged `CASE`s, the kitchen-sink module, and
the early-`RETURN` row, whose fall-through arm is now a default arm because a
fall-through no longer exists.

**W2.4 a pointer array cannot see a record declared later in the module** — **done**

**Found while building the collector** (W10.2), and *not* caused by it: the same
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
Two causes, both in the emitter's type naming and ordering:

1. **ordering.** `type_rank` gives an array `100 + rank(elem)` and a record `200 + …`,
   and a pointer ranks 0, so `ARRAY 2 OF P` is emitted *before* the record it points
   at. A pointer field imposes no order — the comment in `type_rank` says so — and
   neither does a pointer in an array; what such a use needs is a *forward*
   declaration, which is what is missing.
2. **naming.** naming a composite happens on first *visit*, and the visit reaches
   `R` through `P`'s pointer arm while `R` is not yet declared, so it is named
   `T3__Anon_1` — while the interface and every later use call it `T3__R`. A
   pre-pass over the module's declared types would know the name before any visit;
   that is the same shape as the deliberate "name on visit so recursive types
   terminate" rule, which a pre-pass keeps.

A sibling of the same root cause, and the worse one: a *cross-module* pair
(`TYPE P* = POINTER TO Node; Node* = RECORD … END`) names it `Def__Anon_1` in the
header but `Def__Node` in the interface, so the header and the `.sym` disagree.

Done when: a pointer array of a same-module record compiles and runs, the
cross-module pair's header and interface agree on the name, and both are pinned —
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

**W2.3 R16: a structured value argument inside a short-circuit operand is copied eagerly** — **done**
Why: a record argument inside `AND`/`OR` whose own index expressions contain
calls is copied one step earlier than Oberon evaluates it — a documented
deviation, but a deviation.
Done when: the copy happens where the operand is evaluated (or the design
argues why it cannot), with a test that observes the order. **Done**: inside a
`&&`/`||` operand the copy's declaration is hoisted and the copy is a comma
expression whose value is the copy's address — `((rt_rec_copy(&tmp, &(src), …),
&tmp))` — so the copy, and a call in the actual's own selector, happen where the
language evaluates the operand. `e2e`'s `copy-inside-shortcircuit` counts the calls
in both directions (none when short-circuited, one when not) and traps under the
old code.

---

## W3 — toolchain, driver, packaging

**W3.1 `-Ochecks` / `-Ochecks=n`** (M6, §9.4, R6) — **done**
Why: the design says runtime checks are on by default and compiled out with
`-Ochecks=n`; in fact `cgen` emits `rt_trap` unconditionally and the driver has
no such flag.
Files: `crates/cgen/src/cgen.rs` (an option threaded to the three check sites: index, `NIL`
dereference, type-`CASE` no-match), `crates/obc/src/bin/obc.rs`, `e2e/run.sh`.
Done when: a bounds violation traps by default and runs without the check under
`-Ochecks=n`, both covered by suite checks; R6's `FOR`-at-`MAX(INTEGER)` note is
resolved in the same pass (the trap is one of the checks the flag turns off).
**Done**: `-Ochecks=n` removes the index check, the `NIL` check, a guard's tag test
and the type-`CASE` fallback — the four rows in `e2e` assert that the same module
traps by default (with the offset) and exits 0 without them, and that the emitted C
of the unchecked build has no `rt_index`/`rt_nil`/`rt_guard_is` at all. The divisor
checks stay, because they keep the arithmetic *defined* rather than unchecked, and
`ASSERT` is a statement rather than a check; both are stated in §9.4. R6 is
resolved as defined-by-the-report rather than checked — see its row.

**W3.2 interface-file versioning** (§7, "prototype codec") — **done**
Why: the format starts at `MODULE <name>` with no version, so a stale interface
from an older compiler is indistinguishable from a current one.
Files: `crates/sema/src/sym.rs` (both directions), `crates/sema/src/sema.rs` (the error).
Done when: a version line is written and checked, a wrong-version interface is
refused with a located message, and the round-trip tests cover it. **Done**:
`sym::FORMAT_VERSION` is written as the second line (so a reader knows what it is
reading before any declaration) and checked on every parse; a differing version or
a missing line is refused with a message that says to recompile, pinned by
`sym::tests::refuses_an_interface_from_a_different_version`. The two hand-written
e2e interfaces and the fixtures carry the line; in the checker's own fixtures it is
supplied by the helper, since those fixtures are about the language.

Two later findings, from review of the driver's *diagnostic* path rather than the
format: a refusal was keyed by the file's name, so a hand-renamed `Alias.sym`
holding `MODULE M` was looked for under `Alias` and the reason went unreported —
it is keyed by the module its own header names now, readable even when the rest of
the file is not. And a module whose *C* was found skipped the refusal report
entirely, because the driver's early path is not taken when the file is; the
refusal is now also reported from the checker's diagnostic, which is the one place
that knows an interface is needed whatever backs it, and the two paths are
exclusive so neither can report it twice. Both are covered: the e2e keeps
`an-unrelated-refused-interface-is-not-reported` beside
`a-refused-interface-that-is-needed-is-explained`.

**W3.3 continuous integration** — **done**
Why: there is no `.github/` and no CI; the gate is currently a human habit.
Done when: a workflow runs the four suites on a clean checkout, with a C
compiler that has ASan/UBSan (the golden suite runs plain *and* sanitised; `CC`
overrides the default gcc) and a Rust toolchain no older than the one
`Cargo.lock` pins. **Done**: `.github/workflows/gate.yml` runs the list on
`ubuntu-latest` — a build step first, because the suites drive
`target/debug/obc` — then unit tests, golden (plain and sanitised), csmoke, e2e,
fuzz (regression list, traps, sanitizers, coverage) and clippy at `-D warnings`.
Every command in it passes on this machine, which is the evidence available here;
the YAML is parsed, and the one thing a clean runner has that this machine does
not is a *fresh* gcc, which is also why `CC` is respected throughout.

**W3.4 README at the repository root** — **done**
Why: the only entry point is DESIGN.md inside the crate; a reader arriving at
the repository sees no statement of what it is.
Done when: a short README gives the build line, the gate commands, and the two
documents (`oberon/DESIGN.md`, `oberon/PLAN.md`). **Done**: the repository root
has `README.md` with the layout table, the build and gate commands, the driver's
four verbs, the emitted C's build line and why `-fwrapv` is in it, and a pointer
to W6.2 for the fuzzer's state.

---

## W4 — designed with an escape hatch, not built

**W4.1 `SYSTEM`** (R5, report §11.1) — **done**
Why: `crates/sema/src/sema.rs` used to reject `IMPORT SYSTEM` outright ("module SYSTEM is not
available in this implementation (report 11.1)"). Report §11.1 defines five
portable entries and one proper procedure, all of them deliberately outside the
type rules:

| entry | signature (report §11.1) | meaning |
|---|---|---|
| `ADR(v)` | any variable → `INTEGER` | address of `v` |
| `SIZE(T)` | any type → `INTEGER` | size of `T` in bytes |
| `BIT(a, n)` | `(INTEGER, INTEGER) → BOOLEAN` | bit `n` of `mem[a]` |
| `GET(a, v)` | `a: INTEGER`, `v`: any basic type | `v := mem[a]` |
| `PUT(a, x)` | `a: INTEGER`, `x`: any basic type | `mem[a] := x` |
| `COPY(src, dst, n)` | all `INTEGER` | copy `n` consecutive words, `src` → `dst` |

The report also lists implementation-specific extras (`VAL(T, n)`, `ADC`,
`SBC`, `UML`, `COND`, `LED`). We refuse those *by name* ("SYSTEM.ADC is not
implemented") rather than silently, and do not implement them.

Step 1 — resolution. `IMPORT SYSTEM;` resolves to a builtin interface; no
`SYSTEM.mod` is read, and one on the search path is shadowed (documented, since
a user supply would be a different language). The entries are not ordinary
procedures: `ADR`/`GET`/`PUT` take a *variable* (an address is taken),
`SIZE`/`VAL` take a *type*, so this needs the shape of §5 D2 — one production,
dispatched by name in the checker — not five magic signatures.

Step 2 — the rules to add to the §8.5 catalogue. `GET`/`PUT`'s variable must be
of a basic type (a record or array is an error); `SIZE`'s argument must be a
type name, not an expression; `ADR`'s must be a designator whose address exists
(a variable, not a value parameter of structured type); `COPY`'s count is in
words. All of them are unsafe by design: no bounds check, no type check, and
`-Ochecks` must not add any — SYSTEM is the escape hatch, not a place to be
careful.

Step 3 — codegen, inlined (the `ob_sys_` prefix stays reserved for the runtime):
`ADR(v)` → `(intptr_t)(uintptr_t)&v` — *not* the `(int32_t)` first written here:
that truncates a stack address and the word access sign-extends the truncation, so
the round trip faults on a 64-bit host (measured; R31, DESIGN §9.10); `SIZE(T)` →
`(int32_t)sizeof(<C type of T>)`, the layout the record descriptors are built
beside, which the C compiler folds; `BIT(a, n)` →
`((*(const uint32_t *)(uintptr_t)a >> ((uint32_t)n & 31u)) & 1u) != 0`;
`GET`/`PUT` → a word load/store through `(uint32_t *)(uintptr_t)a`, narrowed to
the *typed* operand's width (`GET`'s variable, `PUT`'s value — the report names no
other type); `COPY` → `memcpy(dst, src, (size_t)n * 4u)`. `VAL` is **not** lowered:
with the other RISC extras it is refused by name, which is what step 2 says.

Step 4 — decisions to record (the report is silent, so DESIGN §13 grows):
* the address model. Report §11.1's `mem[a]` is a *word* array; we define an
  address as a **byte** address, so `ADR(v)` is `&v` and `COPY`'s `n` counts
  words → `n * 4` bytes. One sentence in §9.4, and the deviation is visible.
* `SIZE(T)` is the byte size including padding, consistent with the descriptors.
* a module that imports SYSTEM is not portable, and its interface file records
  the import, so a client can see it.

Step 5 — tests, as built. `e2e/Sys.mod` uses every entry and asserts each result
against a hand-computed expectation (`SIZE` of the basic types, an array and a
*stride* that ties `SIZE(R)` to the padded extent, an `ADR`→`PUT`→`GET` round trip
both on module statics and on *locals* — the stack address R31 is about — `BIT` on
a word written that way including the masked count, and `COPY`'s word count with
the words around the moved ones checked unchanged). `csmoke` executes the emitted
snippets (address round trip, `BIT`, `GET`/`PUT` truncation and mask width, the
word count and the `SIZE` extent). The e2e block also runs the located negatives:
the RISC extras refused by name, `GET(a, record)`, `PUT(a, array)`, `SIZE(expr)`,
`ADR(1)`, `GET(a, constant)`, an argument count, and `SYSTEM.x` without `IMPORT
SYSTEM`; plus the shadowing of a `SYSTEM.mod`/`SYSTEM.sym` pair on the search path
and the byte-identical C under `-Ochecks=n`.

**Done** — the tests above pass, and every claim below was executed:

* `DESIGN` §9.10 documents the lowering and the address model (one sentence of it
  in §9.4, the decision in §13's list, the deviation as R31); §8.4 states that
  `SYSTEM` is not a predeclared identifier; §1's out-of-scope bullet is gone, with
  SYSTEM in the in-scope list; §8.5's row 53 is the by-name refusal and rows 55-60
  carry the entries' rules; R5 is "implemented, and the RISC extras still refused
  by name".
* The gate: `e2e` (its `system-*` rows), `golden`, `csmoke` (its `system-*`
  cases), `cargo test` (the catalogue rows and the four new `check_tests`, the
  `SYSTEM.SIZE` reclassification case) and `cargo clippy -D warnings` all pass.
* **Not in this item**: the generator's SYSTEM fragment. `W6`'s own text asks for
  one, and the fuzzer's mirror lives in `crates/obc/src/bin/obgen.rs`; W4.1's half is the
  compiler's. **The sized accessors** W8.7 proposes (`GET8`…`PUT32`) are
  deliberately not taken (§9.10): the report's table has none, and the address
  keeps the host's width instead, so a byte access needs no separate entry.
* **One deviation from this item's own step 3**, found while writing its test and
  recorded as R31: the address is `intptr_t` in the expression, not the
  `(int32_t)` first prescribed, because the `INTEGER` spelling faults on a stack
  address on a 64-bit host.

**W4.2 garbage collection** (R3) — *superseded in form by W10.1, which carries the
same study and states the variables the decision turns on; kept here as the
original entry point. The design study is W4.5 and
comes first*
Why: `NEW` maps to `malloc`; a tracing collector is what real Oberon systems
have and what long-running programs notice. The hatch is `rt_new`, which the
document claims can be replaced without touching generated code.
Prerequisite to settle first: root discovery — the frame layout must let the
collector find the pointers it needs, which is a design question, not a coding
one.
Done when: the design states the root model, and a stress program runs under
`-Ocollect` without leaking or crashing.

**W4.3 the crate split of §2** (`crates/syntax`, `crates/sema`, `crates/cgen`,
`crates/obc`) — **done**
Why: §2 prescribes a split by phase; the prototype is one crate with `src/*.rs`.
It is mechanical because the phase interfaces are three plain data types, and it
pays off when a phase grows — `syntax` is the only crate that may know LALRPOP.
Done when: the four crates exist with the same tests and suites green. **The mapping,
so it is mechanical when someone does it** (the modules as they stand, 19k lines in
one crate):

* `crates/syntax` — `lexer.rs`, `ast.rs`, `oberon.lalrpop` and its `build.rs`. §2's
  rule that this is the only crate which may know LALRPOP is the point of it.
* `crates/sema` — `resolve.rs`, `sema.rs`, `types.rs`, `sym.rs`, `const_eval.rs`
  (depends on `syntax`).
* `crates/cgen` — `cgen.rs`, `layout.rs`, `clif.rs`, `oberon_h.inc`,
  `oberon_runtime.inc` (depends on both). §2's names for these files (`emit.rs`,
  `layout.rs`, `runtime.h`, `mangle.rs`) are its sketch of the same shape; ours are the
  ones that exist, and the divergence is a naming one.
* `crates/obc` — `bin/obc.rs`, `bin/obgen.rs` and `bin/oberon-front.rs` (the
  generator depends on nothing, which is why it can sit here; `oberon-front` is
  the front-end CLI the e2e suite drives as `../target/debug/oberon-front`).

`oberon/Cargo.toml` becomes the workspace manifest with those as members, so the
binaries stay in `oberon/target/debug/` — which means **no suite path changes**, and
the CI workflow's `--manifest-path oberon/Cargo.toml` keeps resolving. The tests move
with their modules (they are unit tests in the same files), and the shell suites do
not know any of this happened.

**Done.** The four crates exist under `crates/{syntax,sema,cgen,obc}` with a virtual
workspace manifest at `oberon/Cargo.toml` (`members = ["crates/*"]`, resolver 2), so
the binaries still land in `oberon/target/debug/` and no suite or CI path changed.
Two moves the note above did not name were forced by the edges and are recorded here:

* `Diagnostic`, `Severity`, `dedup_diagnostics` and the diagnostic renderer moved out
  of `sema.rs`/`lib.rs` into `crates/syntax/src/diag.rs`. A lexical or parse error *is* a
  diagnostic, so its type has to be visible to the parser; putting it in `syntax` is
  what lets the checker import it without a cycle and still keeps `sema` and `cgen`
  from naming LALRPOP.
* `rendered_type` (the `.sym` type reader) moved from `sema.rs` into `syntax`, beside
  the grammar it calls; `sema` reaches it as `syntax::rendered_type`.

The façade (`emit_c_full`, `emit_object_full`, `layout_table_full`, `check_module*`)
and the four test modules are `crates/obc/src/lib.rs`; the two front-end tests that
read a path had one fix — `const_eval.rs`'s folder-vs-C check read
`golden/constexpr.tsv` relative to its manifest, so it reads
`../../golden/constexpr.tsv` now that the crate sits one level down.

**Done** — the numbers are identical to the one-crate tree: `cargo test` **81 passed**;
`cargo clippy --all-targets -- -D warnings` clean at the workspace; `bash golden/run.sh`
PASS, ASan/UBSan clean; `bash csmoke/run.sh` **0 failures**; `bash e2e/run.sh`
**126/0** and `BACKEND=clif bash e2e/run.sh` **126/0**; `bash fuzz/run.sh` and
`BACKEND=clif bash fuzz/run.sh` exit 0. `cargo build|test|clippy --manifest-path
oberon/Cargo.toml` now name the workspace manifest and cover every member, so the CI
workflow resolves unchanged.

**Its back ends are crates of their own too**, which is what §2's split by phase asks
for: `crates/clif` holds `clif.rs` and `layout.rs`, and `cgen` is the C back end and
nothing else. The one inward edge is `clif → cgen` — symbol mangling and the `Checks`
type, which the two back ends must agree on because an object from one links against a
translation unit from the other. The suite numbers above are unmoved by it. Worth
knowing for the next reader: a module named after its own crate is a resolution trap
(`use x::{x, …}` shadows the crate name for the rest of that module), so the moved
files are reached by path.

**W4.4 a `.sym`-only fast path** — **deferred, measured** (the condition it waits for has not fired)
Why: `obc build` has no up-to-date check (nothing in the driver looks at
timestamps), so every module of the import graph is re-checked on each run. A
timestamped interface would skip the front end for unchanged modules.
Only worth doing if build times become a complaint. **Measured**, because the item
conditions itself on a complaint: `obc check` of `e2e/Torture.mod` — lex, parse,
resolve and check, the whole front end on the largest module in the tree — takes
**0.04 s**, while `obc build` of the same program (front end, emit, and `cc` over
four modules) takes **0.73–0.98 s**. The front end is about 5% of a build, so a
`.sym`-only fast path would save a few hundredths of a second per module and the
complaint it waits for is not here. What *would* move build times is at the C end —
not recompiling objects whose source has not changed, or `ccache` — which is a
different design and a different item, and not needed either at these sizes. The
item stays open, now with the number that says when to revisit it.

**W4.5 the collector's design: a study, before any code** — **research, recorded as
DESIGN §15; the implementation is W10.2**
Why: GC is the one item in W4 whose *interface* decides its cost. A collector
needs two things from the compiler that we do not have yet, and getting them
wrong is expensive to undo, so the question is a design question first — and the
reference implementation answers most of it if you read it rather than guess.

What is already verified from the sources (this session, so the study starts
from facts rather than folklore):
* Project Oberon 2013 **does** ship a mark-sweep collector, in `Kernel.Mod`
  (`Mark*`/`Scan*`). Its type descriptor carries the object's **size** and a
  **`-1`-terminated list of pointer-field offsets** (read at `descriptor + 16`),
  which is exactly how it knows where the pointers in an object are.
* its roots are a **vector of pointer slots passed in by the caller**
  (`Mark*(pref)` walks it four bytes at a time), with any value below `heapOrg`
  treated as `NIL` — so the compiler/kernel must be able to *enumerate* roots;
* marking uses **pointer reversal** (a down/up walk that turns the object graph
  into its own stack), so a collection needs no auxiliary stack;
* sweeping rebuilds **segregated free lists** (32/64/128/256 bytes) and keeps an
  `allocated` counter;
* on our side: objects already carry a `desc` pointer (`(*q).desc = &TD_Fr__R`),
  but `ObDesc` is only `{ base, id, name }` — no size and no pointer offsets;
  module variables are plain static globals, so the global root set is free; and
  a non-nested procedure's locals are ordinary C locals, which is the one place
  where roots are *not* enumerable today.

The questions the study has to answer, each with what to read:
1. **Roots** — the crux. (a) precise: emit a per-frame pointer map and spill
   pointer locals into a described frame, so the collector can follow `sl` and
   the frame (this is what the reference compiler must do; confirm how
   `ORG.Mod`/`ORB.Mod` describe a frame, and whether the Project Oberon book's
   collector chapter says so — *not yet verified*); (b) conservative: scan the C
   stack for words that look like heap pointers (Boehm-style), which composes
   with a descriptor-driven *heap* and needs no frame discipline, at the price
   of pinned objects and hidden bugs; (c) explicit frames for every procedure
   (extend today's nested-only `sl` scheme). Recommend one with numbers: code
   size, allocation-path cost, and what each does to `-Ochecks` and to a trap.
2. **Descriptors** — add size and the pointer-offset list (and the array case:
   whether the element type is a pointer, plus the run-time length for open
   arrays), mirroring the reference layout so the code is recognisable. Then the
   cross-module constraint: an imported type's layout must reach a client's
   collector, so this is also a **`.sym` format** question (W3.2) and the format
   must be stable across modules.
3. **Trigger and policy** — collect on allocation failure inside `rt_new` with a
   retry, then `NIL`, which is what DESIGN §8.6 specifies for a failed `NEW`; a
   private `rt_collect()` for tests; a growth policy; and an `allocated` counter
   asserted at exit, which is a leak check for free (the reference keeps one).
4. **What it breaks, honestly** — `SYSTEM` (W4.1) can hide the only reference to
   an object in an `INTEGER`, so a *precise* collector is unsound for programs
   that do that: document the convention (Wirth's systems have the same caveat)
   or take the conservative root scan; *this* paragraph said the sanitised suites
   would have to run with the collector **off**, because ASan cannot see inside a
   collector-managed heap — **W10.2 measured that to be false, and the opposite is
   true**: ASan keeps locals in a fake stack (`detect_stack_use_after_return`),
   so a conservative walk misses every local and silently frees live objects, and
   the collector had to be *fixed* rather than avoided (the runtime turns that
   option off and unpoisons the region it reads). The stress blocks therefore
   build with the sanitizers *on*, and keeping them on is what found that defect;
   §15's ASan note records the contract it implies. Moving/compacting and
   generational collectors are out *because* they invalidate addresses that
   `ADR`/`PUT` may have published — the study should state that rather than leave
   it implied.
5. **The harness, which already exists** — allocation-heavy generated programs
   (W6.1), a mode that collects at every `NEW`, and the `allocated`-counter
   assertion give a stress test without writing one.

Method: read the reference sources and the book chapter, read VOC's tracing
collector and its finalizer list (the survey in W8 found both), and for the
conservative option read Boehm's paper; then write the study as a new section in
DESIGN (next to §8.6, which currently says `NEW` is `malloc`) with a tradeoff
table and a recommendation.

Done when: that section exists and answers all five questions with a source per
claim; §13 gains the decision (adopt a design for W4.2, or leave GC out with the
study as the record); the *cost* is enumerated per code path (allocator,
descriptor, one safepoint per `NEW`, frame handling); and W4.2 is updated to
reference the chosen design rather than the open question. Nothing is implemented
in this item — W4.2 is the implementation.

---

## W5 — error recovery

**W5.1 declaration-level recovery** (§5 D7) — **done**
Why: today a missing `;` between two `CONST` declarations produces one located
message and stops. The grammar *cannot* express recovery there — four shapes
were tried and all four are rejected with a local ambiguity on the error token,
because the report lets each declaration section be empty.
Two ways out, both outside the current grammar:
1. expose the declaration non-terminals as their own LALRPOP entry points
   (`pub ConstDecl` …) and let a driver-side loop parse `;`-separated chunks;
2. hand-write the declaration-section parser and keep LALRPOP for statements and
   expressions.
Done when: a missing `;` in a declaration list still reports the errors *after*
it, with a test that fails today. **Done**, by option 1: `ConstDecl`, `TypeDecl` and
`VarDecl` are public LALRPOP entry points, and the driver reads a broken section one
chunk at a time — lexing, cutting at a `;` at nesting depth zero (a `;` inside a
`RECORD` or parentheses is not a separator), and asking each chunk's parser. Only
chunks that are wrong say anything, and the scan runs only when the module parse has
already failed, so a clean parse pays nothing and cannot be second-guessed. The
test `a_broken_declaration_list_still_reports_what_follows` fails without it: one
missing `;` in a `CONST` list and one in a `VAR` list now produce three located
errors (`unexpected `b``, an incomplete declaration, `unexpected `y``) instead of the
first alone. Boundary, stated rather than implied: this reports *diagnostics*, not an
AST — the module parse is still fatal there, so nothing after it is type-checked.
That is the price of option 1 over a second, hand-written declaration parser.

**W5.2 cascades, an error limit, and the baseline that pins them** — **done**
Why: W5.1 is the *hard* half of recovery (LALR(1) will not let us resynchronise in
declarations at all). The half that is cheap has never been measured, so here is
the measurement — one small module per defect class, run through `obc check`
today:

| probe | diagnostics today | verdict |
|---|---|---|
| `TYPE T = Unknown; VAR a: T; BEGIN a := 1 END` | **1** | already right: the failed type poisons `a`, so the assignment says nothing |
| `CONST c = Zz;` then `TYPE T = ARRAY c OF …`, plus two statements using `c` | **1** | already right, same reason |
| `r.m := 1; r.k := 2; r.n := r.m` on a record without those fields | **3** | already right: one per missing field, and the read of `r.m` does **not** add a second error to its assignment |
| one file with six defects (two `x := ;`, two uses of an undeclared `Zz`, plus clean statements) | **5** | right: no diagnostic is a *consequence* of the recovery, and the syntax errors do not hide the semantic ones |
| **three uses of the same undeclared name** (`Zz := 1; Zz := 2; x := Zz`) | **3** | **fix: report the first use and note the rest** — this is the one repetition the checker still produces |
| **sixty broken statements in one body** | **60** | **fix: an error limit** — nothing stops a flood |

### the commonest defect: a missing `;`

Measured at every site a separator can go missing, plus the `;`-first message
work that came out of it **[landed]**:

| site | today | after the message work |
|---|---|---|
| between statements in a body, next starts with an identifier (`x := 1` ⏎ `y := 2;`) | 2 diagnostics: the syntax error, then the *real* error after the skipped statement; the recovery **discards the next statement**, so that statement's own errors are never reported | the two notes now read together: `expected ;, END, DIV, MOD, OR or IN (12 other token(s) would do as well)` and ``the parser skipped `y := 2` to carry on; a later error may be a consequence`` — the first note names the missing token first, the second names the innocent statement that was lost |
| between statements, next starts with a keyword (`x := 1` ⏎ `IF …`) | same shape, and the keyword makes it unambiguous to a reader at a glance | same |
| before `BEGIN` (`VAR x: INTEGER` ⏎ `BEGIN`) | one fatal error: recovery cannot resynchronise in declarations (W5.1) | `expected ; or END` now leads with `;` |
| between `CONST`/`VAR`/record-field/formal-parameter declarations | one fatal error each | same, with `;` leading |
| between procedures (`END P` ⏎ `PROCEDURE Q;`) | one fatal error | same |
| in an import list (`IMPORT Texts` ⏎ `Oberon;`) | one fatal error | same |
| between case arms (a missing `|`) | one fatal error | same |
| an *extra* `;` between declarations (`VAR x: INTEGER;;`) | one error (`unexpected ;`) | same |

Three things were learned here, and two of them are why the compiler now says
what it says:

1. *The grammar cannot keep the innocent statement.* Tried and rejected, with
   evidence: letting the error token stand in for a statement separator
   (`<s:Statement> <e:!> <rest:StatementSequence>`) produces **4 LALRPOP
   conflicts**, the same wall as W5.1. So a missing `;` in a body is *reported*,
   not *repaired* — and the honest fix is to say exactly what was discarded,
   which is what the recovery note now does. **[landed: `git diff` of
   `crates/syntax/src/oberon.lalrpop` is empty; the experiment was reverted]**
2. *A diagnostic used to hold one note.* `Diagnostic::with_note` **replaced** the
   note, so a recovered error showed the recovery note and silently dropped the
   expected-token note — the reader lost "what would have fitted". `Diagnostic`
   now has `notes: Vec<String>` (which is what DESIGN §10 said all along) and
   renders every one. **[landed; `several_notes_are_all_rendered`]**
3. *`;` leads the expected list.* The set comes from LALRPOP in its own order,
   so the commonest defect used to be answered with "expected DIV, MOD, OR, IN…".
   `;` is now first when it would have fitted. **[landed;
   `a_missing_separator_is_reported_with_both_notes`, and the e2e check pins it]**

Both of those landed, so the item is closed:

1. *First-use suppression for a name* — **landed**. The checker reports an
   undeclared name once and counts the rest onto that first diagnostic
   (``= note: 2 further uses of `Zz` are suppressed``); three uses of one name
   are now one diagnostic, two names are still two, and `one_mistake_is_reported_once`
   (`one_mistake_is_reported_once`) asserts both at once — including the
   double-lookup case that started it.
2. *An error limit* — **landed**. `--ferror-limit N` (default 20, `0` = no limit)
   in both CLIs, warnings never dropped, exit code unchanged, and below the limit
   nothing changes: the small e2e cases still assert exact counts, the flood case
   asserts the ceiling and the suppression line, and `zero-means-no-limit` proves
   `0` means no limit.

Deliberately *not* in this item, with why: declaration-level resynchronisation
(W5.1 — the grammar cannot express it), a second parse pass to recover from a
fatal module-level error (the one located message is honest and cheap), and any
"reparse the tail" machinery (the probes above show the simple behaviour is
already accurate).

Test cases, which are half the point of the item:
* the three tests landed above (`a_missing_separator_is_reported_with_both_notes`,
  `a_missing_separator_in_a_declaration_names_what_followed`,
  `several_notes_are_all_rendered`) cover the message side; what is still open
  is pinning the *counts* for every row of the two tables;
* the counts in the two tables above are pinned where they matter: the
  repeated-name rows and the flood row are tests now, the missing-`;` sites are
  the three message tests landed earlier, and the poison rows are
  `a_failed_type_or_constant_poisons_its_uses`. A wider `recovery_tests` table,
  one row per site as `(source, expected (line, column, kind))`, is still worth
  having if the checker's range of failures grows.
* **a shrinker**, now the binding constraint on the residue: `--per-statement`
  localises a mismatch to one statement, but that statement can sit inside a
  procedure of 25 nested loops, calls and `CASE` arms. `obgen --shrink SEED` should
  delete statements and simplify expressions while the mismatch persists (each
  candidate re-run), which turns a seed into a minimal case the way the residue's
  first finding became `LSL(1, 32)`.
* `e2e` checks that assert a *ceiling* rather than an exact count for the messy
  classes (the sixty-statement flood, the six-defect file), because the ceiling
  is the property that matters.
* the generator's second job (W6.1 already exists): inject exactly one defect
  into a valid program — delete a `;`, replace an identifier with an undeclared
  one, change a declaration's type — and assert four things: no panic, at least
  one diagnostic, at most K diagnostics, and the *first* diagnostic within a few
  lines of the injection. The last two are what a cascade would break.
* tests that the poison behaviour named above is intentional, not incidental —
  **landed** as `diag_tests::a_failed_type_or_constant_poisons_its_uses` (a bad
  `TYPE` and a bad `CONST` each produce exactly one diagnostic, and two missing
  field reads produce two, with no follow-on error on either assignment), plus
  `the_error_limit_truncates_errors_and_never_warnings` for the limit itself.

Done when: the table is pinned by tests, `-ferror-limit` exists and is tested in
both CLIs, the defect-injection mode runs clean over a few hundred seeds, and
DESIGN §10 states the policy — what is suppressed, the poison rule, and the
limit's default — since that section currently describes only the rendering.
**Status**: everything except the defect-injection mode (W6.1's generator work)
is done; §10 states the policy; both CLIs take the flag; 18 `diag_tests` and 29
e2e checks pass.


---

## W6 — the random program generator, and what it found

**W6.1 the generator** — **implemented: `crates/obc/src/bin/obgen.rs`, `bash fuzz/run.sh`**
Why: the three hands-written suites only test what their author thought of. A
generator writes programs that are *known* to be well formed and *known* to
compute a particular value, so any disagreement is a bug rather than a judgement
call.
How it avoids being circular: `obgen` builds a small IR, renders it as Oberon-07,
and interprets the same IR with its own arithmetic, so the expected value of
every module variable comes from the generator, not from the compiler. The
emitted module ends with one check per variable —
`IF m0 # 42 THEN fail[1] := 0 END;  (* check m0 expected 42 *)` — and `fail` is
an `ARRAY 1 OF INTEGER`, so a mismatch traps with the source offset of the
designator (DESIGN §9.4). No output is needed from the program, and the checks
cannot be optimised away. A seed reproduces a program exactly, on any machine.

Covered today: `INTEGER` variables and constants, `+ - * DIV MOD`, `ABS`, the
report's shifts (`LSL`, `ASR`, `ROR` — there is no `ROL`), relations as
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

**W6.2 the first run: the emitted C is not warning-clean** — **done**
Why it matters: the documented build line (`-std=c11 -O2 -fwrapv -Wall -Wextra
-Werror`, DESIGN §9.1) is what the e2e suite uses as its guarantee, so emitted C
that a *legal* program provokes a warning from is a bug in the compiler, not in
the program. Evidence: 25 of 25 seeds failed, all of them in the C compiler and
none in the front end — the checker accepted every generated program — with
three distinct causes:

1. `-Werror=overflow` (106 messages). A constant subexpression that overflows is
   emitted as C constant arithmetic: seed 12 emits
   `(-((0 - 2147483647) - 1))` for `-(MIN(INTEGER))`. The value C computes is
   the wrapped one, so this is a warning rather than a miscompile, but the
   *contract* is that the emitted C compiles. Fix: fold constant subexpressions
   before emission (best), or add `-Wno-overflow` to the build line, which the
   design can defend because `-fwrapv` already declares wrapping intended.
2. `-Werror=unused-parameter` / `unused-but-set-parameter` (13 messages). An
   Oberon parameter the body never reads is emitted as an unused C parameter.
   Every existing suite happens to avoid the case. Fix: emit `(void)p;` for a
   parameter the body does not read.
3. `-Werror=tautological-compare` (1 message). A comparison of two constants is
   emitted as such. Fix: fold it, like (1).

Done when: `bash fuzz/run.sh` passes for a few hundred seeds and joins the gate,
with one regression fixture per cause in `e2e/` (or a row in `Regress.mod`) so
the suites catch a return of any of them. **Done**: six causes, each with a
fixture — the constant overflow (`Regress.mod` prints `2147483647 * 2`), the
unused parameter and the unused local (`e2e`'s `unused-entities-still-compile`
builds a module with an unread variable, parameter and procedure), the
tautological comparison (`-i > -i` and `(i + 1) < (i + 1)`), a self-comparison
whose operands contain *calls* (`ABS(i) = ABS(i)`), and the *order* of operand
evaluation (four rows in `Regress.mod`). The seventh — gcc proving a *symbolic*
overflow, `(MAX - m2) + (MAX + m2)`, which no value fold can reach — is answered
by `-Wno-overflow` in the documented line, whose justification is the same as
`-fwrapv`'s (§9.1). Evidence: `fuzz/regressions.txt`'s nine seeds, seeds 1..100,
200 seeds from 1000 and 400 from 5000 all pass; `e2e` is 55 checks. **Progress**: the fold, the parameter
marks and the named-`CONST` lookup landed, and each landing changed the signal:

| state | C rejections | value mismatches | passing |
|---|---|---|---|
| first run | 25 | 0 | 0 |
| after the fold and the parameter marks | 0 | 24 | 1 |
| after W6.5 (the shift semantics) | 0 | 23 | 2 |
| after the named-`CONST` fold and the self-comparison fold | 1 (a self-comparison, now folded) | 22 | 2 |
| after the mirror's `FOR` semantics was corrected | 1 | 3 | **21** |
| after the review of those commits (fold limits, `rt_abs`, the universe scope, initialise-with-literals, `-O2`) | 0 | 3 | **22** |

The jump is the interesting part, and it was a *generator* bug that the report
settles: §9.8 defines `FOR v := beg TO end … DO S END` as equivalent to
`v := beg; WHILE v <= end DO S; v := v + inc END`, so the control variable ends at
`end + inc` — which is exactly what the emitted C's `for` header leaves. The
mirror stopped at the last body value, so every program that read a control
variable after its loop disagreed with a compiler that was right. `--explain`
(`obgen --explain VAR --base SEED`, the trace of one variable after each
top-level body statement) is what found it: it showed the mirror's value becoming
the expected one inside a `CASE` whose taken arm ran a `FOR`, and the control
variable was in the expression that followed.

The residue is down to three seeds, all self-check traps, and their divergence is
*inside* the recursive function `R`: `m0 := m0 + R(3)` is the statement that moves
the variable whose check traps, `--per-statement m0` narrows it to that single
statement, and the mirror's `R(3)` is 0 while the compiled value differs. `F1`'s
body — 25 lines of nested loops, calls and `CASE` arms — is the suspect, and
reading it has stopped paying: the next step is a shrinker (below).

Earlier in the residue, four seeds were located:
* one is still a C rejection: a self-comparison whose operands contain *calls*
  (`f() = f()`), which `is_plain` deliberately does not fold because a call may
  differ per call and may have side effects, and which gcc warns about anyway
  (`-Wtautological-compare`). Fix: evaluate the operands into temporaries in
  source order and compare those — which C cannot warn about, and which is
  *better* than a bare comparison since Oberon is left to right and C's argument
  order is unspecified. That is the same temporary mechanism §9.5 already uses for
  structured value arguments (R16).
* three are self-check traps whose divergence is inside the recursive function
  `R` (`m0 := m0 + R(3)` is the statement that moves the variable the check traps
  on), and `R`'s body is `acc := F0(...) * (m2 * m11) + R(n - 1)` where `F0`
  *writes* `m2` — which the mirror reads left to right after the call. The
  hypothesis, now DESIGN **R23**, is operand evaluation order: C does not fix it
  and Oberon's reference compilers read left to right. A minimal reproduction
  (a recursive function calling a procedure that increments a module variable,
  multiplied by an operand that reads it) gives the left-to-right answer under gcc
  at `-O0`, `-O1`, `-O2` and `-O3`, so the hypothesis is *not* demonstrated yet —
  the next step is to make the fuzzer trace *inside* a call (`--explain` follows
  the module body only), and to run the fuzzer against the documented `-O2` build
  line rather than the `-O0` it uses for speed.

`(b)` in the earlier list is *not* a cause: `(void)p;` marks a parameter used even
when the body only assigns it (verified in C, gcc accepts both orders).

**W6.5 the generator found a real bug: `LSL`/`ASR` masked their count** — **fixed**
Why it matters: the report's own table (§8.2.2) defines `LSL(x,n)` as `x * 2^n`
and `ASR(x,n)` as `x DIV 2^n`, so a count of 32 or more is *defined* — 0 for a
logical shift left, 0 or -1 for an arithmetic shift right — while only the
*rotation* is a mod-32 operation. The runtime helpers, the constant folder, the
`csmoke` spec (which named the wrong rule) and four document mentions all said
"count mod 32", so `LSL(1, 32)` returned 1 instead of 0.
Evidence: a generated program's self-check trapped; the generator's mirror
follows the report, so the mismatch was the compiler's, and `LSL(1,32)` on its own
settles it.
Fixed in `crates/cgen/src/oberon_h.inc` (both helpers), `crates/sema/src/const_eval.rs` (both folds and
its doc), `csmoke/smoke.c` (the copied helpers, the case that encoded the masking,
and four new cases), `golden/lower.c` and `golden/constexpr_check.c` (the
second-pass recomputation), `golden/constexpr.tsv` (regenerated), and the four
document mentions; DESIGN §13 R22 is the record. All suites green afterwards.
This is the first defect the generator found that the three hand-written suites
could not: they all agreed with the wrong rule.

**W6.3 language details the generator had to learn** — **recorded, not a compiler bug**
The first runs were also a conformance test of the *generator*, and the
compiler's located diagnostics made each one obvious. Kept here because they are
exactly the details a hand-written corpus forgets: an identifier has no `_`
(report §3); `REPEAT … UNTIL expr` has no `END`; a case label is
`integer | string | qualident`, so it cannot be negative, and `CASE` has no
`ELSE` — the default arm is an arm with an empty label list (§10); `MIN`, `MAX`
and `ROL` are not predefined procedures (§10.2), and DESIGN §13's prose used
`MAX(INTEGER)` in three places, which is now corrected.

**W6.4 a gap the generator could not work around** — **decided: the deviation is
recorded in DESIGN §5 D8**
The report's `case = [CaseLabelList ":" StatementSequence]` allows a *default*
arm written as a bare statement sequence, `| S`. Our grammar requires the labels
and the `:` to appear together, so `| S` is rejected. This is not an oversight:
`CaseArm = labels? (":" body)?` cannot be extended to `| labels? ":" ? body`
without a conflict, because a statement sequence and a label list share a first
set (a procedure call statement starts with the same qualident a label may be),
and LALR(1) cannot look two tokens ahead — the reference compiler's recursive
descent can. Options: a hand-written case-arm parser, a two-token lookahead
trick, or documenting the deviation and requiring the empty-label-arm spelling
`| END`. Doing nothing is also defensible (no Oberon-07 program *needs* a default
arm), but the decision should be written down in DESIGN §5. **Decided and
recorded**: the grammar keeps `CaseArm = labels? body?`, the default arm is spelled
`| : S` (verified: `| : S` is accepted, `| S` is rejected with a diagnostic naming
the missing colon), and DESIGN §5 D8 states the conflict, the options and why this
one was taken.

---

**W6.6 the generator's three standing failures** — **done**

`fuzz` reports three seeds failing (7, 9, 17 of 25), each as a program whose own
self-check traps — a *value* the module computed differently from the value the
generator says it must hold — at `fail[1] := 0`, the deliberate out-of-range index
each check uses to fail loudly. None of the three uses an open array or `NEW`, so
they are not from W7.4's fixes; the arithmetic they do is the shortcut family
(`F0`, `LSL`, `ASR`, `DIV`, `ABS`) that R22 touched, which makes the model rather
than the code the first thing to check: the generator must model `LSL(x,n)=x*2^n`
for a count at or beyond the word size, and if it models the mask instead, its
expectation is wrong, not the program's value.

Done when: each seed is either fixed in the compiler or its expectation corrected
in the generator, with the reason recorded — and the seeds are in the gate, not
merely reproducible by hand. **Done**: fixed in the *compiler*, and the reason is
DESIGN R23 with its mechanism in §9.5 — operands and arguments are now evaluated
in source order. The three seeds are in `fuzz/regressions.txt`, and the same file
carries the two seeds that found the argument case (1098, 1116).

Progress, by instrumenting seed 17 (the module's checks replaced with `Out.WriteInt`
of each variable, so the program's values could be read beside the generator's):

* Of the twelve checked variables, **one** differs: the program computes `m0 = 5`
  where the model says `3`. Everything else agrees, so the divergence is in one
  chain of statements, not in the generator's arithmetic at large.
* The generator's *model* of the shifts is right (it follows the report: `LSL` and
  `ASR` of 32 or more are 0 or -1), and so is the runtime: a C probe of
  `rt_lsl`/`rt_asr` gives `LSL(1,33)=0`, `LSL(1000,33)=0`, `ASR(-1000,33)=-1`,
  `LSL(3,32)=0`, `ASR(-8,2)=-2`, `LSL(1,31)=MIN(INTEGER)`. R22's semantics are
  intact, on both sides, so neither is this failure.
* The last statement that writes `m0` is `m0 := (m0 + R(3))` — the exact shape
  DESIGN R23 records — and the one before it is
  `m0 := (F0(F0(MIN)) + LSL(F0(MAX), 33))`, whose *both* operands call `F0`, a
  procedure whose body writes module variables (`m0`, `m8`) and contains a
  `REPEAT` and a `CASE 1 OF 1:`. R23's minimal reproduction does not fire at any
  `-O` level, so the suspect is now inside `F0` itself rather than in the order of
  `+`.
* Next: the same instrumentation inside `F0`'s body (print `p0`, `acc` and the
  module variables at each iteration), which will separate "the emitter evaluates
  a `REPEAT`/`CASE` wrongly" from "the emitter evaluates the operands in the wrong
  order".

## W7 — memory safety: evaluate it, then test it

Why: the design's promise is that a memory access in emitted code is either
provably in bounds by the type system or guarded by a trap that names the source
offset (§9.4, §8.6, R14) — with `SYSTEM` as the only deliberate escape. That
promise has never been audited end to end, and R20 showed what happens when such
a guarantee is assumed rather than tested. Probing it while writing this item
found two holes (W7.1, W7.2) and confirmed two static checks are in place.

**What is answered today** (probes, all in `fuzz/findings/` style modules):

| question | answer |
|---|---|
| an out-of-range *constant* index, `a[10]` on `ARRAY 4 OF INTEGER` | accepted at compile time, traps at run time: `index out of range (at byte 62 of the source)` |
| an out-of-range *variable* index | same check: every index goes through `rt_index(i, n, at)`, constants included — there is no static bounds analysis |
| `ARRAY 5` assigned to `ARRAY 4` | **static error**: `cannot assign an ARRAY 5 OF … to an ARRAY 4 OF …` (report §9.1) |
| a 4-character literal assigned to `ARRAY 3 OF CHAR` | **static error**: `cannot assign a 4-character string to an ARRAY 3 OF …` |
| a failed *type guard* | traps with the offset, and the pointer spelling tests `v != NULL` first (§9.5) |
| a **plain dereference of `NIL`** (`p.x := 1`, `p^ := p^`) | **nothing**: no check is emitted, so the process dies of SIGSEGV with no message — see W7.1 |

**W7.2 a guard on a VAR record parameter of an *imported* type** — **open, found by
`e2e`'s mixed-back-end rows (the layout question that produced them)**
Why: the reclassification of `b(T0)` — guard or call, D2/D3 — works for a parameter
whose record type is declared in the same module and not for an imported one, so the
one legal way to reach an imported type's descriptor from a client is refused:
`PROCEDURE P*(VAR b: Def.Base); BEGIN b(Def.Ext).x := 1 END P` reports "a designator
cannot be applied twice: the result of a call cannot be selected from" (§8.1), while
the identical procedure with `Base`/`Ext` declared locally checks clean. Fixing it
needs care — a guard mistaken for a call is worse than a refusal — so it is its own
item, and the shape belongs in `e2e` with the refusal removed.
Done when: that procedure checks, runs, and its guard reads the descriptor of the
imported type.

**W7.1 a plain pointer dereference is not `NIL`-checked — done**
Why: `rt_trap`'s own comment in the emitted header lists its reasons as
"bounds, NIL, case, ASSERT", and §9.5 checks `NIL` on the *guard* path, but the
emitter's dereference lowering (`Mem3__p->x = 1`) has no check at all. Evidence:
`p := NIL; p.x := 1` exits 139 (SIGSEGV) with empty stderr, where every other
violation of the same class reports a located trap. `p^ := p^` does not even
crash — the C compiler is free to do anything with it, which is the other half
of the problem.
Fix, in `crates/cgen/src/cgen.rs`: lower a pointer access through a `NIL` test, using the
same comma-expression trick §9.5 already uses for guards where the access is a
*read* (`(rt_nil(p, "… at byte N"), p->f)`) and a hoisted statement where it is
an *lvalue* (`(rt_nil(p, …), p->f) = x` is not valid C), with a new `rt_nil`
helper beside `rt_guard_is`. Decision needed: check every dereference (a trap
per access, the safe and consistent choice) or only the ones whose pointer is
not provably non-`NIL` — we have no such analysis, and pretending otherwise is
how the hole got here. Recorded in DESIGN §13 either way.
Done when: field and `^` dereferences of a `NIL` pointer both trap with the
designator's offset, both spellings are in the e2e trap matrix, and the emitted
C for the guarded case still compiles under the documented build line. **Done**,
and the implementation shape matters: `rt_nil` is a *macro*, not a helper function,
because the result has to keep the operand's pointer type (a `void *` cannot be
selected from) and because `rt_nil(p, at)->f` must remain an lvalue so an
assignment to a field of a pointer target still compiles — which is the case the
hole bit. Both spellings trap with the offset (`e2e` checks
`nil-field-dereference-traps` and `nil-caret-dereference-traps`), an emission test
asserts the check is present for a dereference and absent for a pointer assignment
or a `NIL` test, and the decision the item asked for is taken: every dereference
*whose operand is side-effect-free* is checked. **Remaining**: an operand with a
side effect — `a[Next()].x`, where the index expression calls something — is not
checked, because `rt_nil` reads its operand twice and a second evaluation would
change which element is dereferenced (DESIGN R26). The check is left out there
rather than emitted wrong; finishing it means hoisting the operand into a
statement, which is the emitter's temporary machinery (R16) and wants the same
treatment as the fold's call rule.

**W7.2 a constant out-of-range index is not diagnosed — **done** (warn, and still
trap)
Why: `a[10]` on `ARRAY 4 OF INTEGER` compiles, and `e2e/Trap.mod` *relies* on
that (its comment says the check must trap and say where). Nothing is wrong with
the run-time behaviour — it traps, with the offset — but a provably wrong index
is a program bug that a compiler can name for free: the index is folded already
(`rt_index(((int32_t)10u), 4, 62)` appears in the emitted C), so the folder knows
both the value and the length.
**Done** as decided: the compiler *warns* (not errors) when the folded index is
outside `0 .. LEN-1`, `Trap.mod` carries the warning on the way to its trap, and
`diag_tests::a_constant_index_out_of_range_warns_and_still_traps` pins both halves
— the warning *and* the emitted check, so "the compiler can see it is wrong" and
"the check still traps" are both promised and both tested. Declining is also defensible — Wirth's compilers
do not diagnose it — but then §13 should say so, because "the check traps" and
"the compiler can see it is wrong" are different promises.

**W7.3 the audit (the main deliverable)** — **done**
Why: "every access is safe" is a claim about *all* the places the emitted C
touches memory, and it should be a table, not a feeling. Write it into DESIGN
§9.4, one row per access kind, with what makes it safe and which test exercises
it: array index (fixed and open); record field, including through an extended
record and a guard; pointer dereference (`^` and `.`) including `NIL` (W7.1);
value-parameter copy and whole-array/record assignment (`rt_rec_copy`,
`rt_strcopy` — R14's exact-length case); string literal initialisation; the
frame and the static link (R7); descriptor reads for a type guard and for a
type-`CASE` (including a stale or corrupted `id`); `NEW` and a failing `malloc`
(R3); and the trap path itself.
Done when: the table exists, each row names a test, and every row's "safe
because" is either the type system or a helper visible in the emitted C. **Done**:
DESIGN §9.4 carries the table — twelve rows from array indexing to the trap report
itself — and it is honest about the two exceptions rather than implying
completeness: `rt_new`'s failure path has no test yet (W7.4's allocation hook) and
the NIL check is omitted for a side-effecting operand (R26).

**W7.4 the trap matrix in `e2e`** — **done, and it found two compiler defects**
Why: one deliberate violation per kind, each asserting the *offset* reported,
since a trap that says the wrong place is nearly as bad as no trap.
Each in its own module (the first trap aborts the process): index below 0, index
at `LEN`, index in an open-array parameter, a failed record guard and pointer
guard (R15's two spellings), `NIL` field and `^` (W7.1), a type-`CASE` with no
match, `DIV`/`MOD` by zero, and a failing `ASSERT`. `NEW` failing needs a way to
force it — `rt_new` is already the hatch (R3) — so an `OB_ALLOC_FAIL_AT=n`
environment hook makes that row testable too.
Done when: the matrix runs in the gate and each row's expected offset is
asserted, not just "it trapped". **Done**: eleven rows — three index shapes (below
zero, at `LEN`, in an open-array parameter), both guard spellings, a type-`CASE`
with no match, `DIV` and `MOD` by zero, a failing `ASSERT`, the two `NIL`
dereferences and `NEW` failing (through `OB_ALLOC_FAIL_AT`, without which the
report's specified failure is untestable) — each in its own module, each asserting
the message *and* the offset.

It earned its keep immediately, finding two real defects that no other suite could
see (DESIGN R27, R28): a `VAR` open-array parameter was dereferenced as if it were
a pointer to the array (the emitted C did not compile), and an open-array index had
**no bounds check at all** against the hidden length. Both are fixed.

One row stays narrower than it looks: a *failing* guard that also reads the guarded
field cannot be written under `-Werror` at all — gcc sees the read is outside the
allocation (R15) — so the matrix's guard rows use the guard as a value and the
pointer spelling, and R15 keeps its record.

**W7.5 the generator as a memory-safety oracle** — **done**
Why: the self-checks in W6.1 verify *values*; they cannot see a wrong length, a
stale frame pointer or a descriptor read that happens to land inside the heap.
Two additions, both cheap because the machinery exists:
1. run the generated programs compiled with `-fsanitize=address,undefined`
   (`obc build --cflag`, exactly as the golden suite already runs its own
   programs twice) — a clean run is then evidence about the *emitted* code, not
   about the program's own arithmetic;
2. an adversarial mode: emit programs that *deliberately* violate — an index past
   the end, a `NIL` dereference, a guard that cannot hold — where the mirror
   records "a trap is due here" instead of a value, asserting that the program
   traps at the expected offset and corrupts nothing. That is the only test that
   exercises the guard paths rather than the value paths.
Done when: a few hundred seeds are ASan/UBSan-clean, and the adversarial mode's
traps all report their expected offsets. **Done**: `SAN=1` builds every generated
program with `-fsanitize=address,undefined -fno-sanitize-recover=all` and the
sanitizer environment the golden suite uses — 300 seeds from base 2000 and the
nine regression seeds are clean, with leak detection on. `--adversarial` appends
one deliberate violation per program, after the value checks so that a wrong value
still fails first: an index one past `arr`'s end, `pa.x := 1` with `pa = NIL`, a
guard that asks for a pointer to the extension while the object is the base, a
divisor computed to be zero (DIV and MOD), and `ASSERT(FALSE)`. The generator
renders the violation and computes the trap line it must produce from that same
render, byte included, and the harness compares the line rather than merely
requiring a trap — proved by mutation: adding five to the expected byte makes it
report ``expected `… at byte 3655 …`, got: … at byte 3650 …``. 200 seeds from base
3000 and the nine regression seeds pass, and the standard suite now runs the
regression list three ways (values, traps, sanitized) plus 100 coverage seeds and
25 adversarial ones.

---

**W7.6 the narrowing must not cast the designator** — **done**

A designator narrowed by a guard or by a type-`CASE` arm is spelled with the
narrowed C type, so it is no longer an lvalue and `p := NIL` inside
`CASE p OF PS: … END` does not compile (DESIGN R29, found by W7.4). The row in the
matrix has an empty arm to avoid it, which is exactly the kind of accommodation the
matrix exists to remove.

Done: the plain spelling is used where a store is needed, and only where the
narrowed name is the *whole* designator — `field_path` walks *down* the base chain
and is called with the *derived* record, so with a selector after the name the cast
has to stay for the field's offset to be right. That is smaller than the plan above
and needs no coercion change. The guard's check is hoisted to a statement in lvalue
position, which the assignment emitter and the builtins taking a `VAR` argument
flush; a guard that is the whole designator also drops its cast. Rows, all green:
`narrow-assign-to-narrowed` (an assignment to the arm's variable), 
`narrow-field-through-narrowed` (a field write and a field read through the arm's
variable), `narrow-guard-as-target` (a store through both guard spellings, a record
copy from a guarded view, and a narrowed pointer passed to a `PS` parameter).

## W8 — what production Oberon compilers add, and what to adopt

Surveyed: Wirth's own compilers (Project Oberon 2013's RISC compiler `ORP`/`ORB`, and `OP2`), **VOC** (Vishap Oberon), **Oberon+** (`obx`/`obc`), the small-target compilers (AntKrotov's Oberon-07 for MSP430 and STM32, and Oberon-07 for CH32V003/RP2040), and the dialects that stayed in production (Oberon-2, Component Pascal, Active Oberon, Zonnon). Each was asked the same question: what does it offer beyond report §10.2, is it *additive* (a report program still compiles and means the same), and where is the primary source. Nothing here is adopted yet: each item is a proposal with a cost.

Two load-bearing claims were re-verified here rather than trusted, because one survey row had one of them backwards:

* the report's grammar has **no `ELSE`** in a case statement — `CaseStatement = CASE expression OF case {"|" case} END`, `case = [CaseLabelList ":" StatementSequence]` (report, appendix), and our parser rejects `ELSE` **[verified]**;
* adding `ELSE` as a final clause is **conflict-free** in our LALRPOP grammar and reuses the default arm we already have (`labels: None`) **[verified by experiment: 0 conflicts, and probes with and without `ELSE` both check clean]**.

**W8.1 `CASE … ELSE`** — **done**
Every dialect surveyed has it (Oberon-2 §9, Component Pascal §9.11, Oberon+ §Case statements, Zonnon; AntKrotov lists it under its documented differences from the original), and it closes **W6.4**: the report's own default arm — an empty label list followed by a statement sequence, `| S` — cannot be expressed in LALR(1) because a statement and a label share a first set, while `ELSE` can. It also adds **no new keyword** (`ELSE` is already a report keyword, used by `IF`), which is not true of anything else in this wave.
Cost: one grammar clause, one arm with no labels (already in the AST and handled by the checker and emitter as the default), tests. Blast radius: none. **Done**: `ELSE`'s arm is that same node, and two defaults in one `CASE` are refused (DESIGN §8.5 row 63) rather than ordered — which `ELSE` made reachable, since a label-less `|` arm may precede it. Tests: `case_else_is_the_default_arm_and_there_is_only_one`, and `e2e`'s `min-max-and-case-else` and `two-case-defaults-are-refused`.

**W8.2 `MIN(T)` / `MAX(T)`** — **done**
Not in report §10.2 **[verified in the report's own table]**, yet the language needs a way to name an extreme: our folder and generator had to spell `-2147483648` as `(0 - 2147483647 - 1)`, and DESIGN's prose *assumed* `MAX(INTEGER)` in three places until R20's round of corrections. AntKrotov's compiler lists `MIN`/`MAX` among its additional standard procedures.
Cost: two entries in the predefine table, one *type* argument (the same mechanism as a type guard, §5 D2), and folding. Blast radius: none — a program that declares its own `MIN` shadows the predeclared one, which our scoping already does. **Done**: `MIN`/`MAX` are predeclared *names* dispatched by the checker, which records the value by the call's span — the channel `SYSTEM.SIZE` uses for its type — so the emitter lowers the call to a constant and a constant *declaration* folds it like a named constant. The resolver keeps the paren group a call, since the argument is a type. Defined for `INTEGER`, `BYTE` and `CHAR`; a type without an extreme is refused instead of given a zero (rows 61-62). Tests: `min_and_max_are_constants_of_ordinal_types`, `a_declared_min_shadows_the_predefined_one`, and the two `e2e` rows.

**W8.3 per-check granularity for the runtime checks** — **done**
AntKrotov's `-nochk` disables pointer/guard/index/`INTEGER`→`BYTE`/`CHR`-range/stack-overflow checks *individually*; our plan (W3.1) only has an all-or-nothing `-Ochecks`. The finer switch is what the memory-safety work (W7.4) wants to vary per row, and it costs a flag.
Blast radius: none (compiler surface). **Done**: `-Ochecks=<list>` takes the names
`index`, `nil`, `guard` and `case` (`none`/`n` and `all`/`y` for the empty and the
whole set), so a build keeps the checks a test is about and drops the rest; an
unknown name is refused by name. `e2e` pins it structurally — `-Ochecks=nil` emits
`rt_nil` and no `rt_index`, and the reverse — beside the all-or-nothing rows.

**W8.4 early `RETURN`** — **done** (DESIGN §5 D6 is revised by it)
VOC, Oberon-2 §10.1, Component Pascal §10, Oberon+ and Zonnon §7.9 all allow `RETURN` as a statement rather than only at the end of a body, which is what our D6 chose from the report. The stated benefit of that choice — the emitter never needs multiple result-assignment sites — is weak in C, where an early return is `return expr;`, while the usability cost is real: today a procedure cannot return from inside a `CASE`. Proposal: accept `RETURN` as a statement, keep the accumulator shape for the report's spelling, and decide separately whether to relax the single-exit guarantee that D6 documented. **Done**, and the guarantee was *kept* rather than relaxed: `RETURN [Expr]` is a statement, a procedure with a result has one accumulator (`ret__acc`, zero-initialised) and one exit, and each `RETURN` is `ret__acc = …; goto ret;` — so the property D6 documented is now the emitter's doing rather than the grammar's, and the report's tail clause (with the lookahead ambiguity it would have created against the statement form) is gone. The check relaxes to "a function must *contain* a `RETURN` with a value". Tests: `exit-and-early-return` in `e2e` (an early return from an `IF`, from inside a `CASE`, a `CASE` that falls through to a later `RETURN`, and a naked `RETURN`).

**W8.5 `EXIT` for loops** — **done**
`LOOP`/`EXIT` exist in Oberon-2, Component Pascal, Oberon+ and Zonnon. `EXIT` alone (no `LOOP`) removes the flag-variable dance from `WHILE`/`REPEAT`/`FOR` bodies and maps to C's `break`. Against it, from Wirth himself: the 2007 revision removed features "judged duplicative or non-essential" for "regularity, simplicity, completeness, and well-structuredness" — which is why `LOOP` (redundant with `WHILE TRUE`) is *not* proposed, and why `EXIT` should be weighed rather than assumed. Note the keyword cost: `EXIT` is a new reserved word, so an identifier spelled `EXIT` in upper case would collide (Oberon is case-sensitive, so lower case is safe) — the same caveat Oberon+ documents for `WITH` and `LOOP`. **Done**: `EXIT` lowers to C's `break`, which leaves exactly the innermost loop in every form we emit (a `for (;;)` with its guards inside, or a C `for`), so no bookkeeping is needed; `EXIT` outside a loop is refused with a located error (row 64). Tests: `exit-outside-a-loop-is-refused`, and the same `e2e` row walks all three loop forms.

**W8.6 `LONGINT` / `LONGREAL` as real 64-bit types** — **deferred, decision recorded**
Oberon-2 §6.1, Component Pascal §6.1, VOC and Oberon+ (which goes further with `INT8`…`INT64`) all have wider numeric types. But Wirth's own Project Oberon compilers define `LONGINT`/`LONGREAL` as **synonyms** for `INTEGER`/`REAL`, with no widening — so a *real* 64-bit integer is not what a reference implementation means by the name. A C back end gets them cheaply (`int64_t`, `double`), but they touch the type algebra, the folder, the emitter, the `.sym` format and the generator's mirror. Defer until something needs them. **Deferred deliberately**: nothing in the corpus,
the suites or the target programs needs them, and the reference compilers' own
`LONGINT` is a synonym rather than a wider type, so taking the name would mean
something different from what an Oberon programmer expects. Revisit with a program
that needs one, and then as a dialect (DESIGN §13 decision 13).

**W8.7 the escape hatch's scope** — **recorded; consumed by W4.1**, which is done
Every survey found `SYSTEM` growing the same way: Wirth's RISC compiler adds `H`, `COND`, `REG`, `LDREG`, `LDPSR`, `LED`, `ADC`, `SBC`, `UML`; VOC adds `INT8`…`INT64`, `SET32`/`SET64`, `ADDRESS`; AntKrotov adds `GET8/16/32`, `PUT8/16/32`, `SADR`, `WSADR`, `MOVE`, `TYPEID`, `CODE` and, on STM32, `WCHAR`/`WCHR`. Decision to take in W4.1: the report's portable five, plus the *sized* accessors (`GET8/16/32`, `PUT8/16/32`) because a byte-address model needs them, while register/PSR/`LED`/`CODE`/`WCHAR` are target-specific and stay refused by name.

**W8.8 what the survey says to reject** — **recorded; closed**
* syntax conveniences *other than the keyword case above* (which is W8.10): `_` in identifiers, `//` comments, optional end names, single-quoted strings, conditional compilation (AntKrotov, Oberon+) — each makes a program non-portable to a report compiler, and our value is strict conformance;
* type-width *models* (VOC's `-O2`/`-OC`/`-OV`, Oberon+'s fixed-width set) — a dialect matrix multiplies the type algebra; the report fixes `INTEGER` = 32 bits and `REAL` = 32 bits;
* `WITH` — superseded by guards, `IS` and type-`CASE`, and Wirth removed it;
* type-bound procedures/methods (all four dialects) — the report's `IS`/guards/type-`CASE`/procedure variables already cover dynamic dispatch without a vtable model;
* dynamic and multi-dimensional open arrays, `NEW(v, n)`, `LEN(v, dim)` (all four) — they interact with descriptors and, in the systems that have them, with a tracing collector;
* tracing garbage collection and finalizers (VOC, Oberon-2 systems) — finalizers stay rejected (the report has no such concept), and GC is no longer simply "out": its design is being studied first (W4.5) with `rt_new` as the door (R3).

**W8.9 a lesson that is not an extension: our overflow stance is ahead of production** — **recorded; closed**
VOC's C back end does not establish `-fwrapv`, so signed overflow there is undefined C. Our design fixes it deliberately (R4) and R20 is about making that choice *warning-clean* — not about giving it up.

**W8.10 lower-case keywords** — **done** (the keyword fold; the predeclared-name aliases are the separate additive half, not taken)
What: accept reserved words spelled in lower case (`module`, `begin`, `if`, …) so a program can be written in the host toolchain's taste; identifiers stay case-sensitive, so `Foo` and `foo` remain distinct.
Why: both surveyed compilers that care about embedding Oberon in a lower-case codebase do exactly this — AntKrotov's Oberon-07 lists "lower-case keywords" among its differences from the original, and Oberon+ accepts "lower-case forms of reserved words" (together with shorter variants and omitted semicolons, which we are *not* taking) while documenting the reserved-word collision as the price.

The rule that decides the default, and it is worth stating once: **W8.1–W8.5 are additive and may be on by default, because a report program is unaffected; a keyword-case fold is not additive, because the report makes capital and lower-case letters distinct (report §3), so a report program that uses `Begin` or `integer` as an identifier would tokenise differently.** Hence the flag, off by default, and the compiler stays a strict report implementation unless asked otherwise.

Implementation is small and local: keyword recognition is one table and one comparison (`KEYWORDS` and the lookup in `crates/syntax/src/lexer.rs`), so the dialect is a field on `Lexer` (`Lexer::new` keeps the report behaviour) compared with `eq_ignore_ascii_case` instead of `==`. Nothing downstream changes: the token stream is identical, so the grammar, AST, checker and emitter never learn about the dialect.

A second, separate decision that falls out of it: `INTEGER`, `CHAR`, `TRUE`, `FALSE`, `NIL` and the §10.2 procedure names are *predeclared identifiers*, not keywords, so their lower-case forms are aliases in `sema::predefine` rather than a lexer concern — and because a user declaration shadows a predeclared name, that half **is** additive and can be on by default. Worth doing separately from the keyword fold, and cheap.

Cost and risk: the collision surface is "every identifier whose spelling matches a keyword case-insensitively", and the failure must be explained rather than merely reported — the diagnostic for a token where an identifier was expected should say that the current dialect folds keyword case, or the reader will not know why `begin` stopped being a name.

Tests: the existing near-miss table extended with a lower/mixed-case spelling per keyword; a negative test that the default dialect still rejects `begin`; a test that identifiers are not folded; a test that a module declaring its own `integer` or `nil` still compiles with the aliases in place; an `e2e/Lower.mod` built and run through the driver with the flag; and the generator gains a lower-case rendering mode, so the fuzzer covers the dialect end to end.
Done when: the flag reaches both CLIs, the default is provably unchanged (the suites are the proof), those tests pass, the collision diagnostic names the dialect, the dialect is recorded in the interface header (with the version line of W3.2), and DESIGN §3 documents it. **The keyword fold is done**: `-Odialect=report|lower` on both CLIs, `Lexer::with_folded_keywords` behind it, a `DIALECT <name>` line in the interface for a non-default dialect (written only then, so a report interface is byte-identical to before; an unknown name is refused), and the collision note that names the dialect — with the note *not* firing for a reserved word spelled the way the report spells it, since nothing about the dialect explains that. Tests: `lower_case_keywords_need_the_dialect` (the report dialect still rejects `module`; `Foo` and `foo` stay distinct; a lowercase `integer` is an ordinary identifier) and `a_folded_collision_explains_itself`. The generator's lower-case rendering mode is in as well: `obgen --lower` writes the same programs with reserved words in lower case — whole words only, so the predeclared *identifiers* and the generator's own names keep their spelling — and builds them with `-Odialect=lower`, and `fuzz/run.sh` runs the regression list that way on every invocation. The dialect is therefore covered end to end by the corpus, and the values are the same, which is the point: a dialect that changed them would be a different language.

**W8.11 Oberon-2 support: consider it as a dialect, not as dribbled extensions** — **considered; the decision and the mechanism are recorded in DESIGN §5 and §13**
Why: everything in this wave is a piece of Oberon-2, so the honest question is
not "should we add `WITH`?" but "are we becoming an Oberon-2 compiler?". The
delta is now known from Oberon-2's own report (not from summaries):

| Oberon-2 has, Oberon-07 does not | where | already planned here |
|---|---|---|
| `CASE … ELSE` | `CaseStatement = CASE Expression OF Case {"|" Case} [ELSE StatementSequence] END` (§9.5) | **W8.1, recommended** |
| `EXIT`, `LOOP` | statement list (§9) | **W8.5, `EXIT` recommended** |
| `RETURN [Expression]` as a statement | statement list (§9) | **W8.4, recommended** |
| `SHORTINT`, `LONGINT`, `LONGREAL`, `MIN`/`MAX` | §6.1, §10.2 | **W8.6 deferred, W8.2 recommended** |
| `LEN(v, n)`, `HALT` | §10.2 | not planned |
| `NEW(v, x0…xn)`, open arrays beyond formal parameters (pointer bases, element types) | §6.4, §10.3 | W8.8 rejected |
| type-bound procedures: receivers, override on extension, forward declarations | §10.3 | W8.8 rejected |
| type **inclusion** between numeric types (`LONGREAL ⊇ REAL ⊇ LONGINT ⊇ INTEGER ⊇ SHORTINT`), i.e. implicit widening | §6.1 | W8.8 rejected (it contradicts our own rule) |
| `WITH v: T DO … | v: U DO … ELSE … END` | §9.5 | W8.8 rejected |

Two things that table makes obvious, and that shape the decision:

1. **Half of Oberon-2 is already recommended, item by item** (W8.1, W8.2, W8.4,
   W8.5). Each of those was justified on its own merits, and each is additive;
   adopting them is *not* a commitment to a dialect.
2. **The expensive half is not the syntax.** `WITH` is the giveaway: its form is
   literally `WITH v: T1 DO S1 | v: T2 DO S2 ELSE S3 END`, and inside a branch
   `v` is the guarded view — which is precisely what our type-`CASE` already does
   with a type label and a narrowed designator (§8.1, §9.5). It is Oberon-2's
   *spelling* of machinery we have. The genuinely expensive parts are (a) numeric
   *inclusion*, which replaces our explicit "no implicit conversion" rule with
   widening and therefore touches the type algebra, the folder and every
   compatibility check, (b) type-bound procedures, which need a method table per
   descriptor (our `ObDesc` is already per type, so that is its natural home) plus
   override rules and super calls, and (c) heap and nested open arrays, which
   interact with the collector study (W4.5).

The mechanism matters more than the feature list, and it should be decided even
if nothing is adopted: LALRPOP cannot switch alternatives at run time, so a
dialect means *either* two grammars (double the parser maintenance) *or* one
permissive grammar with the checker gating what the dialect allows. The second is
the right choice for us — the checker already owns semantics, and it gives a
diagnostic that names the problem: ``WITH is an Oberon-2 construct; this module
is compiled as Oberon-07 (see --dialect)``. It also means the interface file needs
a dialect field (W3.2) so that a client knows what it is importing.

Scope boundary, stated so it cannot drift: this item is about the *language*.
Eth Oberon's Oberon-2 *library and system modules* are a separate port, and VOC
already occupies that space; our reason to do this would have to be something
else (a particular program to compile, or teaching/analysis uses).

Recommendation: **do not adopt Oberon-2 wholesale now.** (1) take the additive
pieces through their own items as recommended; (2) record the mechanism decision
above in DESIGN, so that a future dialect is a checker-and-emitter job rather
than a parser rewrite; (3) re-open this item against a named trigger — a
specific Oberon-2 program or library we actually want to compile — rather than
against the abstract possibility; (4) leave `WITH`, methods, inclusion and heap
arrays declined (W8.8) until then, since `WITH`'s only advantage over our
type-`CASE` is its spelling and its cost is a new reserved word.

Done when: the decision and the mechanism are recorded (DESIGN §5's dialect note
and §13), the overlapping items stay individually justified, and this item names
the trigger that would change the answer.

Done when: each item above has either landed with tests or been declined in DESIGN §13, and DESIGN §1 points here so a reader knows the extension question has been asked and answered.

---

## W9 — a second back end that emits ELF directly, on Cranelift

**W9.1 research the back end, then build it beside the C one** — **done**
(the research is DESIGN §16, the prototype is §16's second subsection)

Independently checked by the integrator, not taken from the author's report: `obc emit
Elf.mod --backend=clif` writes `Elf.o`, and `file` reads it as *ELF 64-bit LSB
relocatable, x86-64* — machine code for the module with `Elf__init` and `Elf__load`
exported and the only undefined symbols being `ob_rt_index_at`, `ob_rt_div_at`,
`ob_rt_mod_at`, `ob_rt_trap_pos` and `ob_register_commands`. That is the design's ABI
boundary visible in the object itself: the module is compiled, the runtime stays C.
The corpus passes on both back ends (`bash fuzz/run.sh` and `BACKEND=clif bash
fuzz/run.sh`, exit 0 each), and `BACKEND=clif bash e2e/run.sh` stops at the first row
the back end cannot yet meet — 39 check lines against the C path's 101 — which is the
harness for W9.2 rather than a failure of it.

Why: every memory-safety and code-generation finding of W7 has been about the *C*
the emitter writes, not about Oberon-07 — R15 (a guarded read gcc can prove is
outside the allocation), R20 (a `malloc`-typed `NEW`), the `-Werror` classes that
make a correct program unwritable, and the `-fwrapv`-style care the runtime needs.
All of it comes from handing the program to a C compiler that is entitled to assume
the C is defined and that changes its warnings between versions. Cranelift has no
such rights: its integer arithmetic wraps by definition, and its traps are ours to
emit — we call `rt_trap`, so the byte-offset diagnostics the design is built around
are unchanged. A backend that writes ELF is also a backend with no `cc` in the
build at all.

*The crates*, verified at crates.io on 2026-10-15: the family is `cranelift`
0.136.3, `cranelift-codegen` (the generator), `cranelift-frontend` (the IR builder),
`cranelift-module` (functions and data across modules), `cranelift-object` ("Emit
Cranelift output to native object files with `object`" — the ELF path), plus
`cranelift-native` and `cranelift-control`; all `Apache-2.0 WITH LLVM-exception`.
There is **no crate named `cranelift-backend`**; the name to read is the family
above, and `cranelift-object` is the one that writes the object file.

*Research, before any code* — each question gets its answer recorded in DESIGN with
the source it came from:

1. **The object path.** `ObjectModule` + `object::write`, and which `Symbol` and
   `Section` choices carry our static data: the per-type `ObDesc` objects, the
   command table (an array of function pointers) and the generated `main`.
2. **The C ABI boundary.** The runtime stays C, so `rt_*`'s signatures and the
   `ObDesc` layout become a *frozen interface*: confirm Cranelift's calling
   convention is the platform's C one for the shapes we pass (scalars and
   pointers only — records never cross the boundary by value, they are addressed
   through the pointer the language already passes) and how a data object's
   address is materialised for `rt_new`/`rt_is`.
3. **Layout.** Cranelift's IR types are flat; a record is a byte blob reached
   through a pointer, so the offsets stay ours, computed exactly as the C emitter
   computes them. Confirm nothing in the IR builder forces a second layout —
   the descriptor at offset 0 is an interface, not an implementation detail.
4. **Checks and traps.** Every check stays an explicit branch into a runtime call
   (`rt_index`-style helpers or `rt_trap`), so no behaviour depends on a hardware
   fault, and the traceback keeps its source offsets.
5. **Debug info.** Whether to emit DWARF (`cranelift-object`'s debug support,
   `gimli`) or to keep traps as the only locator, as the design already argues.
6. **Integration.** A `--backend=c|clif` switch. The C path stays the reference.
   `golden` and `csmoke` are C-level oracles and stay on the C path; `e2e` and
   `fuzz` are back-end-agnostic and become the Cranelift gate.
7. **What is lost as well as gained.** No preprocessor, no `-O2` to lean on beyond
   `cranelift-codegen`'s own optimisation levels, and no GCC extensions — the
   label-address idiom the descriptor tables use on the C side has no counterpart
   and none is needed.

*Plan*: a lowerer beside `cgen` consuming the same checked AST and `sema` types —
the phase boundary the design already fixes — building SSA per procedure with
`cranelift-frontend`'s `FunctionBuilder` (a `VAR` parameter or an address-taken
local becomes a stack slot; anything else is an SSA value) and emitting through
`cranelift-object`. The runtime interface is kept fixed, so both back ends link the
*same* runtime and a mixed build links — which is what makes a second back end
cheap and keeps the C one honest. Once the prototype is green, the last C
dependency (the runtime itself, plain C over libc) can be lowered by the same back
end, and only then is "generates ELF directly" literally true of the whole program.

Done when: the research questions are answered with their sources in DESIGN; a
prototype builds and runs the `e2e` modules with every row passing; `fuzz`'s seeds
pass against it; and DESIGN records the ABI decision (runtime in C versus lowered)
with the reason it was taken. (When the item was split, the `e2e` half of this
line moved to W9.1's successor — W9.2 — and the acceptance test for W9.1 became
the fuzz corpus, which is what the prototype was finished against.)

**Done.** `crates/cgen/src/clif.rs` lowers the fuzz corpus's fragment to Cranelift IR and
emits one object per module through `cranelift-object`; `obc --backend=c|clif`
(default `c`) writes that object and links it with the same runtime and loader;
`obgen --backend=clif` generates the fragment and `fuzz/run.sh` runs the standard
oracle against it (`BACKEND=clif bash fuzz/run.sh`: **67 programs, 0 failures** —
the 9-seed regression list as a value oracle, as a trap oracle and in the
lower-case dialect, 30 coverage seeds, 10 adversarial coverage seeds). The
research's open questions are answered in DESIGN §16, the prototype in the
subsection after it, and the one thing the research could not have known — the
runtime's checks are `static inline` helpers with no link-time identity, so four
`ob_rt_*` entry points had to be added — is there with the runs that settle the
shift rule (29 of 30 coverage programs carry a count of 32 or more; 6 fail if the
rule is dropped), the `OptLevel`, the module/command-table shape and the one
inconsistency it found between our own two checks (a scalar `CASE` with no
matching label falls through where a type-`CASE` traps and warns; the report is
silent on the case, and the second back end was made to agree with the C path,
which W9.1 does not change — the decision is W2.5's).

**Split, so that the first half is completable rather than open-ended.** The research
half is done. The prototype is now W9.1 proper, scoped to the **fuzz corpus**:
`INTEGER` arithmetic and the shifts, `IF`/`WHILE`/`REPEAT`/`FOR`/`CASE`, procedures,
recursion, and the runtime calls the emitted code makes (`rt_div_at`, `rt_mod_at`,
`rt_trap_pos`, `rt_index`). That is a *whole* milestone rather than a slice of one —
the corpus is the strongest oracle in the repository, since each program carries its
own expected values, and it needs no records, arrays, pointers, descriptors, open
arrays or type-`CASE`. Passing it proves the things the research left open: the
module and object plumbing, the C ABI boundary for the runtime calls, `OptLevel`, and
Cranelift's shift rule, which does *not* match the report's and therefore lands
squarely on the corpus's deliberate large-count shifts.

**W9.4 inlining, if it is wanted** — **open, named by `bench/run.sh`; the payoff is
bounded and the policy is the work**
Why: `bench/CallArray.mod` calls a procedure once per iteration and this back end emits
the call. Cranelift ships the machinery but not the decision — `cranelift-codegen`'s
`inline.rs` calls itself "inlining as a library", says compilation is per-function so it
"does not know which functions are hot and which are cold", and leaves the choice to the
embedder through an `Inline` trait; `Context::inline` is the entry point. Using it here
means keeping every procedure's IR (this back end lowers each into its own `Context` and
defines it, so no callee body survives), writing that policy — small, non-recursive,
call sites one deep is the obvious start — and running the pass before finalising.
Risks to state before anyone writes it: recursion needs a width or depth limit; inlining
does not change `VAR` semantics, since the address is still the caller's, but it does
change code size, and a bad policy costs more than it saves. The payoff is bounded: of
the ~10x gap on that benchmark, inlining is one of three causes, and neither
vectorisation nor the check lowering (W9.3) is affected by it.
Done when: a benchmark says so — the call-heavy ratio moves and `e2e`, the corpus and
the gate are unmoved.

**W9.3 the second back end's runtime checks are calls, not branches** — **open, found
by `bench/run.sh` (DESIGN §16)**
Why: on `bench/CallArray.mod` the second back end runs at ~10x the C back end's time
without checks and ~22x with them, and the checks are the difference: the C back end
emits an inline compare against the bound and jumps to a cold trap on failure, while
this back end calls `rt_index`/`rt_rt` per access. The same divergence covers `NIL`,
the guard and the type-`CASE`. It is a code shape, not a semantics question — the
helper is the semantics — so it does not change what a program computes, only what it
costs.
Done when: `bench/run.sh`'s checked-to-unchecked gap on `CallArray` is much smaller
than the ~2.2x it is now, with the corpus and every suite unmoved.

**W9.2 the rest of the language on the Cranelift back end** — **done**: all four slices
(a–d) are in and `BACKEND=clif bash e2e/run.sh` is **131 passed, 0 failed** — every row
the C path passes, which was the acceptance — with the corpus green on both back ends.
`DESIGN.md` §16 carries the record, including the last row's cause (a frame-held record
local that missed its descriptor walk, whose type-`CASE` then read `NULL + 8`)

**What the mixed-link rows found.** The qualified-call path was complete only in
*statement* position: `Lib.Twice(21)` written where a *value* is wanted was still refused
by name, so a back end that passes every e2e row could not take a result from another
module. `call_value` now has the branch `call_stmt` already had, and the rows below pin
it — four modules, two per back end, one program, linked by `cc`, in both directions.

*Notes from W9.1, so the next session starts from them rather than re-deriving:*

* The refusals the W9.1 lowerer raises are the work list, each at the site it has
  to be replaced (`scalar`/`data_size` for a type, the `Selector` arms of
  `designator_value`/`assign` for one, `call_stmt` for a predeclared name), and
  every one names W9.2 and the byte it was refused at — so the back end widens
  one construct at a time without ever lowering something it has not implemented.
* **The layout is the C ABI's, not ours** (DESIGN §16's layout paragraph is
  corrected): `cgen` emits C structs and lets `cc` place the members, so the
  second back end has to reproduce that layout and *check* it against what `cc`
  reports for the emitted header. This is the first thing to build, because
  records, arrays, pointers, `NEW` and the collector's field maps all sit on it.
* The three shapes that carry over unchanged: data objects with relocations, the
  `ob_rt_*` object-boundary entry points in `oberon.h`, and the
  `__init`/`__load`/command-table model the driver's loader already calls.
* `obgen --backend=clif` renders the scalar fragment and its two blocks (the
  collector's) are C-only: once records and pointers exist, turn them back on in
  `fuzz/run.sh`'s clif mode and widen the adversarial variants, which is what the
  pointer graph's only oracle needs.

What W9.1's prototype leaves out, listed so it is not mistaken for done: records and
their descriptors, `ARRAY` and open-array parameters, `POINTER TO` and `NEW`, type
guards and the type-`CASE`, `SYSTEM`, `REAL`/`CHAR`/`SET`, and the string and record
copies. The constraints are already fixed — the descriptor at offset 0, the runtime's
C ABI, and a layout that is the *C compiler's* rather than ours (see the correction
in the notes above) — so it is a matter of emitting the same shapes through another
IR, and `e2e` is the acceptance test it cannot yet meet.

**Sliced, because the whole of it is more than one session holds** (W9.1's author
stopped rather than half-write it, which is the right call and the reason these are
written down):

* **W9.2a — the layout, and arrays on top of it.** A layout module that computes
  sizes, offsets and alignment the way the C ABI does for the shapes we emit — the
  basic types, fixed arrays, a record whose base is its first member and whose
  descriptor is at offset 0 — plus the test that makes `cc` the authority: emitted
  header in, `sizeof`/`offsetof` out, compared against the computed table. Then
  scalar fixed arrays. Done when that test passes and the corpus's scalar programs
  still do.
  **Done.** `crates/cgen/src/layout.rs` computes it and `obc emit --layout` prints it (a
  diagnostic that stops before emitting anything, so a module whose records this
  back end does not lower yet can still be asked what layout it would get).
  `e2e`'s layout block is the comparison, one `check` per fact: 20 facts over the
  shapes that pin the rules — a record with `INTEGER`/`CHAR`/`REAL`/pointer
  fields, an extension, an array of records, an array of pointers, a trailing
  `CHAR`, `RECORD END` — and every one of them agrees with `sizeof`/`offsetof`
  computed by `cc` from the header the C back end wrote. The comparison is
  *sensitive*: giving a pointer a 4-byte alignment instead of the target's turns
  7 rows red, each naming the type and the field (`layout-Cell.next`,
  `layout-Paired.extra`, `layout-Nodes`, …), and restoring it turns them green.
  Scalar fixed arrays are lowered on that layout — multi-dimensional ones and
  *local* ones included (a local array is a zeroed frame slot, as the C back
  end's `memset` makes it), with one `rt_index` per dimension against that
  dimension's length and the designator's byte: a second subscript out of range
  reports `index out of range (at byte 54 …)` on both back ends. `e2e` 122
  passed/0 failed (101 before), the corpus's scalar programs pass on both paths,
  and the gate is green.

  One thing W9.2c will meet earlier than its own rows: with `BACKEND=clif` the
  suite stops around row 69, at `checks-off-emits-no-check`, which greps the
  emitted `Checks.c` — a file a back end that emits an object never writes — and
  under `set -e` that is where the run ends. The rows that read the emitted *C*
  are about the C back end and have to name it (`--backend=c`) rather than
  inherit `BACKEND`; that is the harness's, not the back end's.
* **W9.2b — records, pointers and the heap.** Descriptor emission matching the C
  emitter's `ObDesc` including the collector's field map, record locals, fields,
  assignment and copies, `POINTER TO`, `NEW` through `rt_new`, and the type-`CASE`
  and guards that read a descriptor. Done when the corpus's *collector* blocks run
  in the clif mode (`fuzz/run.sh`, `obgen --backend=clif` widening accordingly): a
  pointer graph is the only thing that takes those paths apart. **Done**: the clif
  path runs the same seven blocks the C path runs — 86 programs, 0 failures,
  including the collector's stress blocks — and `BACKEND=clif bash e2e/run.sh`
  reaches 56 check lines, which is the distance W9.2c and W9.2d close.
  **Done.** Descriptors, field maps and root registrations are data objects under
  the C back end's names, with every offset from `layout.rs`; `NEW` allocates
  through `rt_new` and initialises the nested descriptors; fields, implicit
  dereferences, `NIL` checks, record copies (`rt_rec_copy`), `IS`, guards and the
  type-`CASE` all read a descriptor through `ob_rt_is`. `obgen`'s fragment mode and
  `run.sh`'s clif-only branch are deleted, so the corpus is one corpus again:
  `BACKEND=clif bash fuzz/run.sh` runs the same seven blocks as the C path —
  **86 programs, 0 failures, exit 0**, the collector's two stress blocks included
  (the collector forced on every allocation) — and the C path is unchanged at exit
  0. Two findings came out of it: a module variable that can hold a pointer must be
  aligned to its *type* rather than to a word (UBSan caught a misaligned load in
  the collector's walk on four regression seeds), and the C back end's `IS` and
  type-`CASE` read through a `NIL` pointer where the report's answer is "no match"
  and the lowered code gives it. `e2e` through clif reaches 56 checks with 9
  failing — the distance W9.2c closes, reported rather than made to pass.
* **W9.2c — the value types, open arrays and `VAR` parameters.** `REAL`, `CHAR` and
  `SET` (with the ordinal and set operations `csmoke` pins on the C side), open
  arrays in both modes with the hidden length, `VAR` parameters, and the string and
  record copies the parameter modes require. Done when the `e2e` rows about values
  and parameters pass under `BACKEND=clif`.
  **Done.** `REAL` is the `f32` with the float instructions (`fcmp` for its
  comparisons, `FLT`/`FLOOR`/`ABS` included, and `FLOOR` saturating where C's cast
  is undefined); `CHAR`/`BYTE` is one byte read *unsigned* wherever it is compared
  or converted (`ORD`/`CHR`, the ordinal order, and `rt_charcmp` for two character
  arrays); `SET` is the `uint32_t` mask (`bor`/`band`/`bxor`/complement, `ob_rt_set`
  for a constructor, the `& 31` bit test for `IN`). A `VAR` actual is the caller's
  address, a structured *value* actual a copy the caller makes, an open array a
  pointer and the hidden length its index check uses, and a string literal a data
  object — `rt_strcopy` into an array, a byte copy for array-to-array. Cross-module
  calls and imported variables came with it, since `Hello`/`Out` need them.
  **`BACKEND=clif bash e2e/run.sh`: 66 checks pass, 3 fail** (from 56/9 at the start
  of the slice; `e2e` on the C path is 126/0), and the three are named: `Torture.mod`
  declares a **procedure variable**, and `GcRoots.mod` needs a descriptor that
  belongs to *another* module's object (an imported record type's `TD_` is emitted
  here rather than imported, which a mixed build must not do). Both are refused by
  name with their byte. The corpus passes on both back ends (86 programs, exit 0),
  and the gate is green.
  *Found here*: DESIGN §9.4's table names an `rt_charcopy` for an array-to-array
  `CHAR` copy, and the header has no such function — the C back end never needed one
  (a full-length copy is a C struct assignment), so the lowerer copies the bytes
  itself and the table's row is one the C side never had to justify.
* **W9.2d — the scope machinery, and every row.** Nested procedures with static
  links, procedure variables, `SYSTEM`, imported record descriptors, and whatever
  rows remain. Done when `BACKEND=clif bash e2e/run.sh`
  passes every row the C path does — 126 as of W9.2b, and the harness is there to
  say so.
  **Done.** `SYSTEM` is in (all six entries on §9.10's address model, with the C's
  implicit conversions made explicit in the IR — an `intptr_t` address compared
  with an `INTEGER` is what needs it), the call through a procedure-valued record
  field is in, and the last row was a *local record in a frame*: a procedure that
  owns a frame keeps its locals in it, and the walk that gives every local record
  its descriptor was collecting only the locals in slots, so a record inside a
  frame was never initialised (ASan put it at `Regress__localRecord__which`,
  `SEGV on address 0x8`). Fixed, and **`BACKEND=clif bash e2e/run.sh`: 126 pass, 0
  fail** — every row the C path passes. The C path is 126/0, the corpus passes on
  both back ends, and the gate is green.

  *Superseded note.* The rest of this entry describes what the earlier runs of this
  slice landed. Nested procedures with static links
  are in (a frame per nesting procedure, the hidden link parameter, link walking
  for an enclosing local, links passed at nested calls, symbols keyed by lexical
  path), with `PACK`/`UNPK`, a `CASE` over `CHAR`/`BYTE` and one block per `WHILE`
  guard along the way. **`BACKEND=clif bash e2e/run.sh`: 119 pass, 7 fail** (the
  earlier 67 was the harness stopping early; that row now reports). What is left:
  a call through a procedure-valued *field* (`Regress.mod`, refused by name), and
  **`SYSTEM`** (four rows: `ADR`/`SIZE`/`BIT`/`GET`/`PUT`/`COPY`, the builtin
  import's interface line, and the search-path shadowing pair) plus one
  cross-module anonymous-type name. The gate is green and the corpus passes on both
  back ends.

  *Superseded note.* The rest of this entry describes the two items landed in the
  previous run of this slice. An interface's record type now *imports* its `TD_`
  rather than defining one, so `IS`, a guard and a type-`CASE` agree across a module
  boundary (`gc-roots-hold-their-objects` is the row that says so; the layout walk
  also had to follow a pointer to its base, and to record a pointer's own layout
  before recursing, or `P = POINTER TO Node; Node = RECORD next: P END` never
  terminates — the two `Gc.mod` rows caught that at once). Procedure values are code
  addresses (`func_addr` for a module-level procedure, `call_indirect` for a call
  through one, with the signature the checker recorded at the call site), which is
  what `Torture.mod`'s `fn := twice; fn(21)` needed. **`BACKEND=clif bash
  e2e/run.sh`: 67 pass, 2 fail** (from 66/3). What is left is the other two:
  **nested procedures with static links** and **`SYSTEM`**, both refused by name
  today and both required by the two remaining rows — `Torture.mod` declares
  `PROCEDURE inner` and reaches `SYSTEM`. The gate is green and the corpus passes on
  both back ends.

**W7.7 a byte for every trap, not only the memory ones** — **done**

The matrix asserted an exact source offset for the index and guard traps, because
those messages carry one, while `DIV by zero`, `MOD by zero`, `assertion failed`
and `CASE: no matching type` carried none — so a user who tripped one had no
location, though the runtime had the offset in hand at every one of those sites.
**Done**: `rt_div_checked`/`rt_mod_checked`/`rt_guard_at` take the expression's
offset and the `CASE` default calls `rt_trap_pos`, all four messages now read
`(at byte N of the source)`, and the matrix's four rows assert the exact line
(`DIV by zero (at byte 55 …)`, `MOD by zero (at byte 55 …)`, `assertion failed
(at byte 19 …)`, `CASE: no matching type (at byte 137 …)`). The two-argument
`rt_div`/`rt_mod`/`rt_guard` remain as wrappers, because `golden`'s hand-written
fixtures call them directly and have no offset to pass.

## W10 — a garbage collector

**W10.1 the collector: the decision, and what it costs the emitter** — **done**
(the design is DESIGN §15; the implementation is W10.2)

Why: Oberon-07 has `NEW` and no `DISPOSE` — the report's §6.4 defines allocation and
says nothing about freeing — so every program this compiler builds leaks by
construction, and a long-running one dies of it rather than of its own logic. No
amount of book-keeping in the front end fixes that; it is a runtime subsystem. Two
things already in the design make it cheap here: there is exactly one allocation
point (`rt_new`, made the only one in W7.4), and every heap object begins with its
own `ObDesc`, so a collector has one place to hook and something to walk.

Research to start from — read it, do not assume it: the Oberon report §6.4 (`NEW`
and the absence of a free); this design's descriptor, runtime and frame sections
(§6.4, §9.2, §10); the chapters on runtime systems and memory management in
`books/` (Appel, *Modern Compiler Implementation in C*; the *Compiler Design
Handbook*); and Wirth's own Oberon compilers, whose `NEW`-plus-descriptor scheme is
the ancestor of this layout. Decide and record, with the reason:

1. **Precise or conservative marking.** A precise mark needs a pointer map per
   type, which the descriptor does not carry today (it holds base, size and a
   name). A conservative mark needs neither that nor a stack map, but it must
   reckon with the values in a frame that are *not* pointers — the hidden
   open-array length is an `int32_t` that may look like one — and with interior
   pointers. Retaining garbage is acceptable; freeing a live object is not. The C
   back end cannot supply a stack map, so conservative is the only option there,
   while the Cranelift back end can (W9's research), which points at: a
   descriptor-driven mark with a conservative stack and register walk first, and
   precise stack maps as the Cranelift back end's follow-up.
2. **What the descriptor gains.** If the mark is precise, the descriptor needs the
   pointer-field information the emitter can already produce — `desc_init` walks
   exactly those fields to install nested descriptors — and the item must say what
   that costs in object size and when the interface format version changes (W3.2).
3. **The root set, stated explicitly.** Module-level variables, the live state of
   the command table, and the stack. Said as sharply: a value parameter's *copy* is
   a root, a `VAR` parameter's original is, an open array's hidden length is not,
   and a `NIL` is not.
4. **When to collect.** Before `NEW` gives up, so that the report's
   `NEW`-yields-`NIL` (§6.4) keeps its meaning with a bounded heap — and so that
   `OB_ALLOC_FAIL_AT` stays a *test* hook rather than the only path to a failing
   allocation. A collection that frees nothing must terminate, not loop.
5. **What it must never do.** Free a live object (the fuzzer's self-checks are the
   oracle), and move objects: the emitter addresses them through raw pointers
   today, so this is a reclaimer, not a compactor.

Plan: extend the descriptor with whatever the chosen design needs, put the
collector in the runtime header (a mark stack, a sweep over the allocated list, the
`rt_new` hook), keep the allocator's single entry point, and drive it from a mode
the tests can force — `OB_GC_STRESS=1`, the shape `OB_ALLOC_FAIL_AT` already has.

Done when: a program that allocates in a loop runs in a bounded heap; the fuzzer's
seeds pass with the collector forced on every allocation; and the design states the
root set and the mark/precision decision, with the reason it was taken. **The
decision half is done and recorded as DESIGN §15**, decided from Wirth's §14.2 (the
three facts a collector needs, and the descriptor that carries two of them),
Mogensen's §12.2 (the root set, which is why a conservative stack walk is needed
where there are no stack maps) and the *Compiler Design Handbook*'s chapter 6 (the
method taxonomy, which rules out the moving collectors for a back end that addresses
objects through raw pointers): mark-sweep, a precise mark driven by a descriptor
that gains the pointer-field offsets, a conservative stack walk now and a precise
one when W9's back end can supply stack maps, and collection before `NEW` gives up.
The implementation is W10.2.

**W10.2 the collector itself** — **implemented: `crates/cgen/src/oberon_h.inc`,
`crates/cgen/src/oberon_runtime.inc`, `crates/cgen/src/cgen.rs`, `crates/obc/src/bin/obgen.rs`, `fuzz/run.sh`,
`e2e/run.sh` (five rows) and one line of `e2e/Texts.c`, whose hand-written
descriptor literal had to list the descriptor's two new fields like any other
positional initializer under the e2e build's `-Werror`**

Implement DESIGN §15: extend `ObDesc` with the pointer-field offsets the emitter
already knows (`desc_init`'s walk), add the mark stack, the sweep over the allocation
list and the `rt_new` hook to the runtime, register each module's pointer variables as
roots, and add `OB_GC_STRESS=1` so the tests can force a collection per allocation.
Settle the three open questions §15 lists — the allocation list's shape (a header
before the payload, or a table), the trigger besides a failing allocation, and how
the stress mode avoids changing a program's answers.

Done when: a program that allocates in a loop runs in a bounded heap; `fuzz` passes
with the stress mode on (its self-checks are the "no live object was freed" oracle);
and the standard suite is green with it off, so the collector costs nothing when it
is idle.

**Done**, and §15 now records each answer with the run that executes it:

* **The list is a header in front of the payload** (`ObBlock`: next, payload size,
  mark), so the descriptor stays at offset 0 of the object and the sweep has both the
  next block and the size an interior pointer is tested against. The descriptor gains
  `const ObFld *fields; int32_t nfields;`, where an entry is
  `{off, stride, n, desc}`: pointer slots (`desc == NULL`, one or a run), or a run of
  records whose own map takes over. A type that reaches no pointer gets no map, so
  marking it is one NULL test. The map is emitted by the same walk `desc_init` uses,
  with `offsetof` expressions, so it follows the C layout the back end does not
  compute for itself.
* **The trigger besides a failing allocation is `OB_GC_LIMIT=n`**, a byte budget on
  headers plus payloads: over it, the collector runs, and `NEW` yields `NIL` if it is
  still over — the report's failure case with a heap that never runs out of malloc.
  A collection that frees nothing ends the attempt; nothing loops.
* **`OB_GC_STRESS=1` collects before every allocation** and changes no answer: only
  unreachable blocks are freed, nothing moves, and which address a later `NEW` gets
  was always `malloc`'s to decide. Roots are each module's own variables, registered
  from `__init` before its body can allocate (a pointer variable's slot, a record
  variable's type, an `ARRAY OF RECORD` as a run); the command table needs no entry,
  holding names and code addresses only, and everything else is the conservative
  stack walk — with a `setjmp` for the registers a frame carries across the call.
  The runtime also collects once at exit, so what a leak checker sees is what the
  program still holds.

Evidence: `e2e`'s six collector rows (1000 rounds of 20 nodes = 20000 allocations in
a 4096-byte heap, completing in the stress, limit and idle modes with the same
checked value; a live chain over the limit reaching `NIL` and counted back intact;
and one root of each kind, including a type whose map lives in another module and
one whose pointer field the base of the object's type declares); the generated
fragment's heap probe, which keeps a chain per root kind across churn; `fuzz`'s two
stress blocks, built with the sanitizers whether or not `SAN=1` is set, because a
freed object whose memory is not reused yet still reads correctly — ASan is what
caught the one defect this walk had while it was being written (under
`-fsanitize=address` locals live in a fake stack the walk cannot see, so the runtime
turns `detect_stack_use_after_return` off and unpoisons the region it reads). The
standard suites are unchanged with the collector idle: no stress, no limit, and the
only path that collects is a `malloc` failure.

**W10.3 collector pauses** — **done: measured, and the bound declined with its reason**
(`OB_GC_STATS=1` and `fuzz/gc-bench.mod` are the instrument and the benchmark; numbers
and the path they point at are at the end of this item)

The collector §15 decides on is stop-the-world, so a pause is proportional to the
live set *plus* the whole heap: marking walks the reachable graph and conservatively
scans the stack, and sweeping walks the allocation list whether an object is live or
not. Two things make the worst case the common one rather than a corner: the trigger
is a *failing* allocation, which is exactly when the heap is fullest, and a deep
recursion lengthens the stack scan in proportion to its depth. For the scale this
compiler is aimed at that is a fine trade — the design says so — but it is a
property a user will meet, so it deserves its own item rather than a footnote.

What to reach for, in order of cost, each with the source that describes it:

1. **Trigger on a threshold**, not only on failure: the same total work spread over
   time, and it removes the grow-to-the-limit-then-pause shape. It needs the heap's
   high-water mark per program, which is the same instrument as (4).
2. **Incremental marking** — the *Compiler Design Handbook*'s chapter 6, §6.5
   ("Incremental and Concurrent Non-Moving Collectors"). Non-moving is what makes
   this reachable at all: without compaction a mutator can keep running while the
   mark advances, with only a barrier at the points where it hands a pointer over.
3. **Concurrent marking**, then **generational** (§6.4) if allocation rates say so:
   generational cuts pause times most and costs write barriers plus the
   intergenerational-pointer bookkeeping, which is a real change to the *emitter*,
   not just the runtime — every store to a pointer field would need it.
4. **Shorter conservative scans** before any of that: an instrument that reports the
   pause distribution (`OB_GC_STATS=1`, the shape `OB_ALLOC_FAIL_AT` and
   `OB_GC_STRESS` already have), so the choice is made from measurements rather than
   from the argument above. The Cranelift back end's stack maps (W9) are the real
   fix for the stack half: they make the scan precise, and they bound it by the
   *live* frames rather than by depth.

Done when: a named benchmark's pause distribution is measured and recorded (worst
and median, with the heap it ran in), a stated bound is met or explicitly declined
with the reason, and the correctness oracle is unchanged — `fuzz` with
`OB_GC_STRESS=1` and the standard suite both still green. "We measured, and this is
the bound we accept" is a legitimate outcome; "it seems fine" is not.

**First step is the instrument, and it is not optional**: §15's collector has no
counter of any kind, so wall-clock numbers cannot separate a pause from the mutator
work between pauses. `OB_GC_STATS=1` reports, per collection, the bytes before and
after, the blocks marked and swept, and the *time spent inside* `ob_collect` — the
shape `OB_ALLOC_FAIL_AT` and `OB_GC_STRESS` already have, and everything it needs is
already in the runtime: the switch goes beside the other two in `ob_gc_env`, the byte
counts are `ob_heap_bytes` before and after, the mark stack gives what was marked and
the sweep loop already distinguishes what it keeps from what it frees, and the clock
is `timespec_get(&ts, TIME_UTC)` — C11, so no feature-test macro and no new dependency.
It writes to *stderr*, so a program's own output is untouched, and a final line at exit
gives the totals. A benchmark then reports a distribution instead of an impression.

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
numbers: 0.28 s is four hundred times a 60 Hz frame. Two things are worth separating
in why. The first is the benchmark's *shape* — 200 000 objects become garbage before
the first collection, so the first sweep walks the whole heap, and that is the 1.03 s
at the larger ceiling, not a steady state. The second is the one that matters: total
time inside the collector is ~4.2 s either way, and it only *moves* between few long
pauses and many short ones as the ceiling shrinks — which is the measurement saying
that the work is what it is and this design does it with the mutator stopped.

So the path is the one §15 and this item list, in order: **trigger on a threshold**
rather than only on a failing allocation, which removes the grow-to-the-limit burst
and spreads the same work; then **incremental marking**, which is reachable at all only
because this collector does not move objects; then concurrent marking, and generational
only if allocation rates ask for it. The numbers above are the baseline those have to
beat, and the knob that exists today — the ceiling — is recorded with them.

**One early observation, recorded with its caveat.** A churn benchmark of my own —
5000 live nodes kept while 200 000 objects are allocated and dropped — ran
*unbounded* in 12.0 s and 12.8 s, at a 2 MB ceiling in 5.7 s and 6.8 s, and at
512 KB in 10.6 s and 7.6 s. The bounded runs being *faster* than the unbounded one is
not the collector being free: it is that reuse beats growth, since the unbounded run
grows the heap to ~6 MB and pays for the pages while the bounded one stays cache-hot.
So the trade this collector makes is memory spent against pauses rather than speed
against safety — and the honest caveat is that those numbers came from a machine that
was compiling at the same time, which is why they are a direction and not a
measurement. The instrument is what turns them into one.

## Ideas not yet in the design (adopt or drop)

- Fuzz the *lexer* with random bytes and random token streams: assert no panic
  and that every diagnostic is located. W6 covers whole programs; this would
  attack the input that has no structure at all.
- Grow the corpus where it is thin: `e2e` has the report's modules, but the
  *diagnostic* catalogue (§8.5's 55 rows) is exercised only by unit tests; a few
  modules per row would test the driver path too.

## Non-goals (deliberate, DESIGN §1)

Optimisation of any kind — the C compiler does it; Oberon-2 and later dialects;
release packaging and publishing; and nothing under `../books/` is versioned —
those PDFs stay on disk and ignored.
