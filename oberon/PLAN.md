# Oberon-07 → C: implementation backlog

This is the open work, derived from `DESIGN.md` itself rather than from
enthusiasm: every item names the section or risk it comes from, the files it
touches, and what makes it done. Paths are relative to this directory.

An item is done when **the four suites and clippy pass** and the evidence it
promises exists as a test or a suite check:

```
cargo test                  # 47 unit tests
bash golden/run.sh          # the golden cases, run twice: plain and under ASan/UBSan
bash csmoke/run.sh          # 67 C-level cases
bash e2e/run.sh             # 26 checks, end to end through the driver
cargo clippy --all-targets -- -D warnings    # not green yet: W0.1
```

`bash fuzz/run.sh` (SEEDS/BASE select the run) is deliberately *not* in that
list yet: it found three defects on its first run (W6.2) and stays out until
they are fixed, or it would be a gate that always fails.

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

**W0.1 clippy is not clean**
Why: the preamble says "clippy clean"; `cargo clippy --all-targets` reports 244
warnings, *all* in LALRPOP's generated parser
(`target/…/out/oberon.rs`, `clippy::ptr_arg` — `&mut Vec` is what LALRPOP's
`ErrorRecovery` signature requires, so it cannot be changed).
Files: `src/lib.rs` (the `lalrpop_mod!` site).
Done when: an `#[allow(clippy::ptr_arg)]` — scoped to the generated module, not
the crate — makes `cargo clippy --all-targets -- -D warnings` exit 0, and the
clippy line is part of the gate above.

**W0.2 two sections are still marked `[design]` although they are executed daily**
Why: §9.2 (type mapping) and §9.3 (records, extension, descriptors) are marked
`[design]`, but `src/cgen.rs` implements them and `e2e`/`csmoke` run them.
Done when: those markers say what is actually unverified (if anything), so that
`grep '\[design' DESIGN.md` returns only the legend.

---

## W1 — the rules the checker still does not enforce

These are DESIGN §8.5's own list of four unchecked things, quoted verbatim.

**W1.1 field visibility across modules** (rows 4 and 14's private-field halves,
report §6.3)
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
Files: `src/types.rs`, `src/cgen.rs`, `src/sym.rs`, `src/sema.rs`.
Done when: the mark travels through the interface, an access to a private field
of an imported record is an error, and `check_tests` has a case per row (each
case names its row number).

**W1.2 row 50's locality half** (report §6.5)
Why: a *local* procedure assigned to a procedure variable is rejected, but one
passed as an *actual* is not — the check exists on one side of a symmetric rule.
Files: `src/sema.rs`.
Done when: passing a nested procedure where a procedure variable is expected is
an error, with a test.

**W1.3 row 54's unused-*procedure* half**
Why: unused imports warn; an unused local procedure does not.
Files: `src/sema.rs` (the `unused_imports` pass is the natural home).
Done when: a module with an uncalled local procedure warns once, the warning is
located, and it is visible through `obc check` (the path fixed this session).

**W1.4 settle §13's open questions and delete them**
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

---

## W2 — known deviations with a real failure mode

**W2.1 R17: the reclassification pass uses module-wide name sets**
Why: `resolve.rs` splits guard-vs-call and folds module qualification *before*
the checker builds scopes, so a local variable that shadows an import alias, or
a nested type name, is classified by the wrong scope. This is a correctness
bug for programs that shadow, not a style matter.
Files: `src/resolve.rs`, `src/sema.rs` (it already builds the scopes the pass
needs).
Done when: a module whose local shadows an import alias checks correctly (or is
rejected for the right reason), with a test; then §13's R17 becomes a note
about the prototype's pass order history rather than a live limitation.

**W2.2 R15: a *failing* guard with the record spelling is statically out of bounds**
Why: `p(S).b` where `p` points at `R` reads through a pointer that cannot hold
an `S`; the guard's tag test traps first at run time, but gcc's
`-Warray-bounds`/`-Wmaybe-uninitialized` can see the read statically and reject
the program under `-Werror`. The pointer spelling has no such read, which is why
the e2e trap case uses it.
Candidate fixes: lower the guarded access through a union of the possible
record layouts, or through an opaque `char *` plus `memcpy` (both keep the trap
first); a local pragma is the fallback, documented rather than silent.
Done when: the same program compiles under both spellings with gcc *and* clang
and still traps with the right offset.

**W2.3 R16: a structured value argument inside a short-circuit operand is copied eagerly**
Why: a record argument inside `AND`/`OR` whose own index expressions contain
calls is copied one step earlier than Oberon evaluates it — a documented
deviation, but a deviation.
Done when: the copy happens where the operand is evaluated (or the design
argues why it cannot), with a test that observes the order.

---

## W3 — toolchain, driver, packaging

**W3.1 `-Ochecks` / `-Ochecks=n`** (M6, §9.4, R6)
Why: the design says runtime checks are on by default and compiled out with
`-Ochecks=n`; in fact `cgen` emits `rt_trap` unconditionally and the driver has
no such flag.
Files: `src/cgen.rs` (an option threaded to the three check sites: index, `NIL`
dereference, type-`CASE` no-match), `src/bin/obc.rs`, `e2e/run.sh`.
Done when: a bounds violation traps by default and runs without the check under
`-Ochecks=n`, both covered by suite checks; R6's `FOR`-at-`MAX(INTEGER)` note is
resolved in the same pass (the trap is one of the checks the flag turns off).

**W3.2 interface-file versioning** (§7, "prototype codec")
Why: the format starts at `MODULE <name>` with no version, so a stale interface
from an older compiler is indistinguishable from a current one.
Files: `src/sym.rs` (both directions), `src/sema.rs` (the error).
Done when: a version line is written and checked, a wrong-version interface is
refused with a located message, and the round-trip tests cover it.

**W3.3 continuous integration**
Why: there is no `.github/` and no CI; the gate is currently a human habit.
Done when: a workflow runs the four suites on a clean checkout, with a C
compiler that has ASan/UBSan (the golden suite runs plain *and* sanitised; `CC`
overrides the default gcc) and a Rust toolchain no older than the one
`Cargo.lock` pins.

**W3.4 README at the repository root**
Why: the only entry point is DESIGN.md inside the crate; a reader arriving at
the repository sees no statement of what it is.
Done when: a short README gives the build line, the gate commands, and the two
documents (`oberon/DESIGN.md`, `oberon/PLAN.md`).

---

## W4 — designed with an escape hatch, not built

**W4.1 `SYSTEM`** (R5, report §11.1) — **the item this plan is currently for**
Why: `src/sema.rs` rejects `IMPORT SYSTEM` outright ("module SYSTEM is not
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
`ADR(v)` → `(int32_t)(uintptr_t)&v`; `SIZE(T)` → a constant from the layout the
record descriptors already compute, which the folder can fold; `BIT(a, n)` →
`(*(uint32_t *)(uintptr_t)a >> n) & 1`; `GET`/`PUT` → a word load/store through
`(uint32_t *)(uintptr_t)a`, masked to the variable's width; `COPY` →
`memcpy(dst, src, (size_t)n * 4)`; `VAL(T, n)` → a bit-pattern-preserving copy
for same-size scalars.

Step 4 — decisions to record (the report is silent, so DESIGN §13 grows):
* the address model. Report §11.1's `mem[a]` is a *word* array; we define an
  address as a **byte** address, so `ADR(v)` is `&v` and `COPY`'s `n` counts
  words → `n * 4` bytes. One sentence in §9.4, and the deviation is visible.
* `SIZE(T)` is the byte size including padding, consistent with the descriptors.
* a module that imports SYSTEM is not portable, and its interface file records
  the import, so a client can see it.

Step 5 — tests. A new `e2e/Sys.mod` uses every entry and prints results against a
hand-computed expectation: `SIZE` of a record and of an array, an
`ADR`→`PUT`→`GET` round trip, `BIT` on a word written that way, `COPY` between
two arrays. `csmoke` covers the emitted snippets (address round trip, `COPY`'s
word count, `GET`/`PUT` truncation width). Negative cases: `SYSTEM.ADC` refused
by name; `GET(a, record)` and `SIZE(expr)` rejected; `SYSTEM.x` without `IMPORT
SYSTEM`; `PUT` into a read-only entity.

Done when: those tests pass, DESIGN §13 carries the address model, §1's
out-of-scope bullet is replaced by a pointer here, R5 becomes "implemented, and
the RISC extras still refused by name", and the generator (W6) has a SYSTEM
fragment whose `PUT`/`GET` round trips and `SIZE` calls the mirror can predict.

**W4.2 garbage collection** (R3)
Why: `NEW` maps to `malloc`; a tracing collector is what real Oberon systems
have and what long-running programs notice. The hatch is `rt_new`, which the
document claims can be replaced without touching generated code.
Prerequisite to settle first: root discovery — the frame layout must let the
collector find the pointers it needs, which is a design question, not a coding
one.
Done when: the design states the root model, and a stress program runs under
`-Ocollect` without leaking or crashing.

**W4.3 the crate split of §2** (`crates/syntax`, `crates/sema`, `crates/cgen`,
`crates/obc`)
Why: §2 prescribes a split by phase; the prototype is one crate with `src/*.rs`.
It is mechanical because the phase interfaces are three plain data types, and it
pays off when a phase grows — `syntax` is the only crate that may know LALRPOP.
Done when: the four crates exist with the same tests and suites green.

**W4.4 a `.sym`-only fast path** — *proposed, not from the design*
Why: `obc build` has no up-to-date check (nothing in the driver looks at
timestamps), so every module of the import graph is re-checked on each run. A
timestamped interface would skip the front end for unchanged modules.
Only worth doing if build times become a complaint.

---

## W5 — recovery beyond statements

**W5.1 declaration-level recovery** (§5 D7)
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
it, with a test that fails today.

---

## W6 — the random program generator, and what it found

**W6.1 the generator** — **implemented: `src/bin/obgen.rs`, `bash fuzz/run.sh`**
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

**W6.2 the first run: the emitted C is not warning-clean** — **open**
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
the suites catch a return of any of them.

**W6.3 language details the generator had to learn** — **recorded, not a compiler bug**
The first runs were also a conformance test of the *generator*, and the
compiler's located diagnostics made each one obvious. Kept here because they are
exactly the details a hand-written corpus forgets: an identifier has no `_`
(report §3); `REPEAT … UNTIL expr` has no `END`; a case label is
`integer | string | qualident`, so it cannot be negative, and `CASE` has no
`ELSE` — the default arm is an arm with an empty label list (§10); `MIN`, `MAX`
and `ROL` are not predefined procedures (§10.2), and DESIGN §13's prose used
`MAX(INTEGER)` in three places, which is now corrected.

**W6.4 a gap the generator could not work around** — **open**
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
arm), but the decision should be written down in DESIGN §5.

---

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
