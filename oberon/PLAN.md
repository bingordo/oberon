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

**W4.1 `SYSTEM`** (R5, report §11.1)
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

**W4.2 garbage collection** (R3) — *implementation; the design study is W4.5 and
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

**W4.5 the collector's design: a study, before any code** — **research**
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
   or take the conservative root scan; ASan, which is a verification mode of ours
   (W7.5), cannot see inside a collector-managed heap, so the sanitised suites
   must run with the collector **off** and the collector gets its own stress
   mode; moving/compacting and generational collectors are out *because* they
   invalidate addresses that `ADR`/`PUT` may have published — the study should
   state that rather than leave it implied.
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

**W5.2 cascades, an error limit, and the baseline that pins them** — **the simple half, measured**
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
   `src/oberon.lalrpop` is empty; the experiment was reverted]**
2. *A diagnostic used to hold one note.* `Diagnostic::with_note` **replaced** the
   note, so a recovered error showed the recovery note and silently dropped the
   expected-token note — the reader lost "what would have fitted". `Diagnostic`
   now has `notes: Vec<String>` (which is what DESIGN §10 said all along) and
   renders every one. **[landed; `several_notes_are_all_rendered`]**
3. *`;` leads the expected list.* The set comes from LALRPOP in its own order,
   so the commonest defect used to be answered with "expected DIV, MOD, OR, IN…".
   `;` is now first when it would have fitted. **[landed;
   `a_missing_separator_is_reported_with_both_notes`, and the e2e check pins it]**

Still open in this item — two small, boring changes, not a rearchitecture:

1. *First-use suppression for a name.* A per-check set of names already reported
   turns three diagnostics into one plus `further errors for "Zz" are
   suppressed` — the phrasing every C compiler uses, and the reason R2/R3 style
   floods happen at all. Cheap because the checker already has the name and the
   span at that point.
2. *An error limit.* `-ferror-limit N` (default 20, `0` meaning no limit),
   counted where diagnostics are collected, with a final
   `N more errors were suppressed` line. It must not change the exit code, must
   not suppress warnings, and must not change anything below the limit.

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
* a `recovery_tests` table in `oberon/src/lib.rs`, one row per probe above as
  (source, expected diagnostics as `(line, column, kind)`), so every count in
  that table becomes pinned: a future cascade or a lost diagnostic fails a test
  rather than being noticed by hand. The two rows marked *fix* start as their
  current counts and change when the fix lands.
* `e2e` checks that assert a *ceiling* rather than an exact count for the messy
  classes (the sixty-statement flood, the six-defect file), because the ceiling
  is the property that matters.
* the generator's second job (W6.1 already exists): inject exactly one defect
  into a valid program — delete a `;`, replace an identifier with an undeclared
  one, change a declaration's type — and assert four things: no panic, at least
  one diagnostic, at most K diagnostics, and the *first* diagnostic within a few
  lines of the injection. The last two are what a cascade would break.
* tests that the poison behaviour named above is intentional, not incidental:
  `ErrorType` must keep suppressing a follow-on error (R1/R4/R7 in the table are
  those tests), and the limit must leave a below-limit run byte-identical.

Done when: the table is pinned by tests, `-ferror-limit` exists and is tested in
both CLIs, the defect-injection mode runs clean over a few hundred seeds, and
DESIGN §10 states the policy — what is suppressed, the poison rule, and the
limit's default — since that section currently describes only the rendering.


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

**W7.1 a plain pointer dereference is not `NIL`-checked — open, a defect**
Why: `rt_trap`'s own comment in the emitted header lists its reasons as
"bounds, NIL, case, ASSERT", and §9.5 checks `NIL` on the *guard* path, but the
emitter's dereference lowering (`Mem3__p->x = 1`) has no check at all. Evidence:
`p := NIL; p.x := 1` exits 139 (SIGSEGV) with empty stderr, where every other
violation of the same class reports a located trap. `p^ := p^` does not even
crash — the C compiler is free to do anything with it, which is the other half
of the problem.
Fix, in `src/cgen.rs`: lower a pointer access through a `NIL` test, using the
same comma-expression trick §9.5 already uses for guards where the access is a
*read* (`(rt_nil(p, "… at byte N"), p->f)`) and a hoisted statement where it is
an *lvalue* (`(rt_nil(p, …), p->f) = x` is not valid C), with a new `rt_nil`
helper beside `rt_guard_is`. Decision needed: check every dereference (a trap
per access, the safe and consistent choice) or only the ones whose pointer is
not provably non-`NIL` — we have no such analysis, and pretending otherwise is
how the hole got here. Recorded in DESIGN §13 either way.
Done when: field and `^` dereferences of a `NIL` pointer both trap with the
designator's offset, both spellings are in the e2e trap matrix, and the emitted
C for the guarded case still compiles under the documented build line.

**W7.2 a constant out-of-range index is not diagnosed — open, a decision**
Why: `a[10]` on `ARRAY 4 OF INTEGER` compiles, and `e2e/Trap.mod` *relies* on
that (its comment says the check must trap and say where). Nothing is wrong with
the run-time behaviour — it traps, with the offset — but a provably wrong index
is a program bug that a compiler can name for free: the index is folded already
(`rt_index(((int32_t)10u), 4, 62)` appears in the emitted C), so the folder knows
both the value and the length.
Decision: warn (not error) when the folded index is outside `0 .. LEN-1` or the
folded string is longer than its target, and update `Trap.mod` to expect the
warning on the way to its trap. Declining is also defensible — Wirth's compilers
do not diagnose it — but then §13 should say so, because "the check traps" and
"the compiler can see it is wrong" are different promises.

**W7.3 the audit (the main deliverable)** — **open**
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
because" is either the type system or a helper visible in the emitted C.

**W7.4 the trap matrix in `e2e`** — **open**
Why: one deliberate violation per kind, each asserting the *offset* reported,
since a trap that says the wrong place is nearly as bad as no trap.
Each in its own module (the first trap aborts the process): index below 0, index
at `LEN`, index in an open-array parameter, a failed record guard and pointer
guard (R15's two spellings), `NIL` field and `^` (W7.1), a type-`CASE` with no
match, `DIV`/`MOD` by zero, and a failing `ASSERT`. `NEW` failing needs a way to
force it — `rt_new` is already the hatch (R3) — so an `OB_ALLOC_FAIL_AT=n`
environment hook makes that row testable too.
Done when: the matrix runs in the gate and each row's expected offset is
asserted, not just "it trapped".

**W7.5 the generator as a memory-safety oracle** — **open**
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
traps all report their expected offsets.

---

## W8 — what production Oberon compilers add, and what to adopt

Surveyed: Wirth's own compilers (Project Oberon 2013's RISC compiler `ORP`/`ORB`, and `OP2`), **VOC** (Vishap Oberon), **Oberon+** (`obx`/`obc`), the small-target compilers (AntKrotov's Oberon-07 for MSP430 and STM32, and Oberon-07 for CH32V003/RP2040), and the dialects that stayed in production (Oberon-2, Component Pascal, Active Oberon, Zonnon). Each was asked the same question: what does it offer beyond report §10.2, is it *additive* (a report program still compiles and means the same), and where is the primary source. Nothing here is adopted yet: each item is a proposal with a cost.

Two load-bearing claims were re-verified here rather than trusted, because one survey row had one of them backwards:

* the report's grammar has **no `ELSE`** in a case statement — `CaseStatement = CASE expression OF case {"|" case} END`, `case = [CaseLabelList ":" StatementSequence]` (report, appendix), and our parser rejects `ELSE` **[verified]**;
* adding `ELSE` as a final clause is **conflict-free** in our LALRPOP grammar and reuses the default arm we already have (`labels: None`) **[verified by experiment: 0 conflicts, and probes with and without `ELSE` both check clean]**.

**W8.1 `CASE … ELSE`** — *recommend, and it fixes something*
Every dialect surveyed has it (Oberon-2 §9, Component Pascal §9.11, Oberon+ §Case statements, Zonnon; AntKrotov lists it under its documented differences from the original), and it closes **W6.4**: the report's own default arm — an empty label list followed by a statement sequence, `| S` — cannot be expressed in LALR(1) because a statement and a label share a first set, while `ELSE` can. It also adds **no new keyword** (`ELSE` is already a report keyword, used by `IF`), which is not true of anything else in this wave.
Cost: one grammar clause, one arm with no labels (already in the AST and handled by the checker and emitter as the default), tests. Blast radius: none.

**W8.2 `MIN(T)` / `MAX(T)`** — *recommend*
Not in report §10.2 **[verified in the report's own table]**, yet the language needs a way to name an extreme: our folder and generator had to spell `-2147483648` as `(0 - 2147483647 - 1)`, and DESIGN's prose *assumed* `MAX(INTEGER)` in three places until R20's round of corrections. AntKrotov's compiler lists `MIN`/`MAX` among its additional standard procedures.
Cost: two entries in the predefine table, one *type* argument (the same mechanism as a type guard, §5 D2), and folding. Blast radius: none — a program that declares its own `MIN` shadows the predeclared one, which our scoping already does.

**W8.3 per-check granularity for the runtime checks** — *recommend*
AntKrotov's `-nochk` disables pointer/guard/index/`INTEGER`→`BYTE`/`CHR`-range/stack-overflow checks *individually*; our plan (W3.1) only has an all-or-nothing `-Ochecks`. The finer switch is what the memory-safety work (W7.4) wants to vary per row, and it costs a flag.
Blast radius: none (compiler surface).

**W8.4 early `RETURN`** — *recommend; this revisits one of our own decisions*
VOC, Oberon-2 §10.1, Component Pascal §10, Oberon+ and Zonnon §7.9 all allow `RETURN` as a statement rather than only at the end of a body, which is what our D6 chose from the report. The stated benefit of that choice — the emitter never needs multiple result-assignment sites — is weak in C, where an early return is `return expr;`, while the usability cost is real: today a procedure cannot return from inside a `CASE`. Proposal: accept `RETURN` as a statement, keep the accumulator shape for the report's spelling, and decide separately whether to relax the single-exit guarantee that D6 documented.

**W8.5 `EXIT` for loops** — *recommend, with the counter-argument recorded*
`LOOP`/`EXIT` exist in Oberon-2, Component Pascal, Oberon+ and Zonnon. `EXIT` alone (no `LOOP`) removes the flag-variable dance from `WHILE`/`REPEAT`/`FOR` bodies and maps to C's `break`. Against it, from Wirth himself: the 2007 revision removed features "judged duplicative or non-essential" for "regularity, simplicity, completeness, and well-structuredness" — which is why `LOOP` (redundant with `WHILE TRUE`) is *not* proposed, and why `EXIT` should be weighed rather than assumed. Note the keyword cost: `EXIT` is a new reserved word, so an identifier spelled `EXIT` in upper case would collide (Oberon is case-sensitive, so lower case is safe) — the same caveat Oberon+ documents for `WITH` and `LOOP`.

**W8.6 `LONGINT` / `LONGREAL` as real 64-bit types** — *defer, with the blast radius stated*
Oberon-2 §6.1, Component Pascal §6.1, VOC and Oberon+ (which goes further with `INT8`…`INT64`) all have wider numeric types. But Wirth's own Project Oberon compilers define `LONGINT`/`LONGREAL` as **synonyms** for `INTEGER`/`REAL`, with no widening — so a *real* 64-bit integer is not what a reference implementation means by the name. A C back end gets them cheaply (`int64_t`, `double`), but they touch the type algebra, the folder, the emitter, the `.sym` format and the generator's mirror. Defer until something needs them.

**W8.7 the escape hatch's scope** — *feeds W4.1*
Every survey found `SYSTEM` growing the same way: Wirth's RISC compiler adds `H`, `COND`, `REG`, `LDREG`, `LDPSR`, `LED`, `ADC`, `SBC`, `UML`; VOC adds `INT8`…`INT64`, `SET32`/`SET64`, `ADDRESS`; AntKrotov adds `GET8/16/32`, `PUT8/16/32`, `SADR`, `WSADR`, `MOVE`, `TYPEID`, `CODE` and, on STM32, `WCHAR`/`WCHR`. Decision to take in W4.1: the report's portable five, plus the *sized* accessors (`GET8/16/32`, `PUT8/16/32`) because a byte-address model needs them, while register/PSR/`LED`/`CODE`/`WCHAR` are target-specific and stay refused by name.

**W8.8 what the survey says to reject** — recorded so the question does not come back
* syntax conveniences *other than the keyword case above* (which is W8.10): `_` in identifiers, `//` comments, optional end names, single-quoted strings, conditional compilation (AntKrotov, Oberon+) — each makes a program non-portable to a report compiler, and our value is strict conformance;
* type-width *models* (VOC's `-O2`/`-OC`/`-OV`, Oberon+'s fixed-width set) — a dialect matrix multiplies the type algebra; the report fixes `INTEGER` = 32 bits and `REAL` = 32 bits;
* `WITH` — superseded by guards, `IS` and type-`CASE`, and Wirth removed it;
* type-bound procedures/methods (all four dialects) — the report's `IS`/guards/type-`CASE`/procedure variables already cover dynamic dispatch without a vtable model;
* dynamic and multi-dimensional open arrays, `NEW(v, n)`, `LEN(v, dim)` (all four) — they interact with descriptors and, in the systems that have them, with a tracing collector;
* tracing garbage collection and finalizers (VOC, Oberon-2 systems) — finalizers stay rejected (the report has no such concept), and GC is no longer simply "out": its design is being studied first (W4.5) with `rt_new` as the door (R3).

**W8.9 a lesson that is not an extension: our overflow stance is ahead of production**
VOC's C back end does not establish `-fwrapv`, so signed overflow there is undefined C. Our design fixes it deliberately (R4) and R20 is about making that choice *warning-clean* — not about giving it up.

**W8.10 lower-case keywords** — *requested; opt-in by default, see the rule below*
What: accept reserved words spelled in lower case (`module`, `begin`, `if`, …) so a program can be written in the host toolchain's taste; identifiers stay case-sensitive, so `Foo` and `foo` remain distinct.
Why: both surveyed compilers that care about embedding Oberon in a lower-case codebase do exactly this — AntKrotov's Oberon-07 lists "lower-case keywords" among its differences from the original, and Oberon+ accepts "lower-case forms of reserved words" (together with shorter variants and omitted semicolons, which we are *not* taking) while documenting the reserved-word collision as the price.

The rule that decides the default, and it is worth stating once: **W8.1–W8.5 are additive and may be on by default, because a report program is unaffected; a keyword-case fold is not additive, because the report makes capital and lower-case letters distinct (report §3), so a report program that uses `Begin` or `integer` as an identifier would tokenise differently.** Hence the flag, off by default, and the compiler stays a strict report implementation unless asked otherwise.

Implementation is small and local: keyword recognition is one table and one comparison (`KEYWORDS` and the lookup in `src/lexer.rs`), so the dialect is a field on `Lexer` (`Lexer::new` keeps the report behaviour) compared with `eq_ignore_ascii_case` instead of `==`. Nothing downstream changes: the token stream is identical, so the grammar, AST, checker and emitter never learn about the dialect.

A second, separate decision that falls out of it: `INTEGER`, `CHAR`, `TRUE`, `FALSE`, `NIL` and the §10.2 procedure names are *predeclared identifiers*, not keywords, so their lower-case forms are aliases in `sema::predefine` rather than a lexer concern — and because a user declaration shadows a predeclared name, that half **is** additive and can be on by default. Worth doing separately from the keyword fold, and cheap.

Cost and risk: the collision surface is "every identifier whose spelling matches a keyword case-insensitively", and the failure must be explained rather than merely reported — the diagnostic for a token where an identifier was expected should say that the current dialect folds keyword case, or the reader will not know why `begin` stopped being a name.

Tests: the existing near-miss table extended with a lower/mixed-case spelling per keyword; a negative test that the default dialect still rejects `begin`; a test that identifiers are not folded; a test that a module declaring its own `integer` or `nil` still compiles with the aliases in place; an `e2e/Lower.mod` built and run through the driver with the flag; and the generator gains a lower-case rendering mode, so the fuzzer covers the dialect end to end.
Done when: the flag reaches both CLIs, the default is provably unchanged (the suites are the proof), those tests pass, the collision diagnostic names the dialect, the dialect is recorded in the interface header (with the version line of W3.2), and DESIGN §3 documents it.

**W8.11 Oberon-2 support: consider it as a dialect, not as dribbled extensions** — *consideration, with a recommendation*
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
