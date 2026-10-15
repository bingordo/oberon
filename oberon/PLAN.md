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

Recently closed, for context: DIAG's located diagnostics and statement-level
recovery (DESIGN §5 D7, §10), the CLI's warning path (a clean module used to
hide its warnings), and this document's move into the crate.

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

**W4.1 `SYSTEM`** (R5, report §11.1)
Why: importing it is a hard error today, which the design chose deliberately.
The hatch is already named: `ADR`, `SIZE`, `VAL`, `BIT` are intrinsics,
`GET`/`PUT`/`COPY` are `memcpy`-shaped, and the `ob_sys_` prefix is reserved.
Done when: `SYSTEM` resolves, each intrinsic is covered by a case, and the
checks reveal what the rest of the compiler assumed about type safety.

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

## Ideas not yet in the design (adopt or drop)

- Fuzz the lexer and parser with random token streams: assert no panic and that
  every diagnostic is located. The recovery work makes panics the interesting
  failure, not wrong parses.
- Grow the corpus where it is thin: `e2e` has the report's modules, but the
  *diagnostic* catalogue (§8.5's 55 rows) is exercised only by unit tests; a few
  modules per row would test the driver path too.

## Non-goals (deliberate, DESIGN §1)

Optimisation of any kind — the C compiler does it; Oberon-2 and later dialects;
release packaging and publishing; and nothing under `../books/` is versioned —
those PDFs stay on disk and ignored.
