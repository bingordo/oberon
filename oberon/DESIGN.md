# Oberon-07 → C: front end and back end design

Target language: **Oberon-07 as defined by *The Programming Language Oberon*, N. Wirth,
revision 1.10.2013 / 3.5.2016** (`../books/Oberon07.Report.pdf`, cited below as *report §n*).
Paths in this document are relative to its own directory, the repository's
`oberon/` crate.

Parser generator: **LALRPOP** (0.23.1), used for the full concrete syntax.
Output: **C11**, one translation unit per Oberon module, compiled by the host `cc`.

The crate and the three suites below back the claims in this document and are
meant to be kept:

| Artefact | What it is | Status |
|---|---|---|
| `src/bin/obgen.rs`, `fuzz/` | **A random program generator used as a self-checking oracle**: it writes a module *and* the value every variable must have (its own interpreter, so no circularity), and a mismatch traps in the module itself | 25 seeds run, 0 survived: it found three defects in the emitted C that the hand-written suites had missed (§13 R20) |
| `.` (this crate) | Working front end: hand-written lexer, the LALRPOP grammar, the AST, the resolver **and checker** (scopes, type resolution, diagnostics), the interface-file codec, the type algebra and the constant folder, all with tests | builds, 55 tests pass, clippy clean |
| `csmoke/` | Every C emission rule of §9 that has a non-obvious semantic, compiled and executed | 67 cases pass, 1 bug in the test spec itself (R11) |
| `golden/` | The report's own procedures hand-lowered to C in the shape §9.6–9.8 prescribes, a two-module link model with command dispatch, and the constant table the folder is cross-checked against — **compiled against the runtime the compiler emits, not a copy of it**; run twice (plain and under ASan/UBSan) | 67 cases pass, 3 design corrections found (R12–R14) |
| `e2e/` | **The whole compiler, end to end**: five Oberon modules (including the report's own `Out` and a hello-world) compiled by this repository's compiler to C, that C compiled by the host `cc` with `-Werror`, the program run and compared with an independently computed expectation — and the same again through the `obc` driver | 26 checks pass; it found the emitter and checker bugs listed in §14 |

Everything marked **[design]** below is not yet implemented; everything marked
**[verified]** was executed in this repository (commands in §14).

---

## 1. Scope

**In scope**

* All of Oberon-07 proper: modules, imports, constants, types (basic, array,
  record with extension, pointer, procedure), variables, the four operator
  classes, all seven statement forms, procedures with VAR/value/open-array
  parameters, nested procedures, the 29 predeclared identifiers of report §10.2.
* One *compilation unit* at a time for the compiler proper: `Foo.Mod` is read
  together with the interface files of the modules it imports, and becomes
  `Foo.c` + `Foo.h` + `Foo.sym`. The *driver* (§9.9) resolves a whole program's
  import graph on top of that and runs the C compiler.
* Diagnostics with byte spans, resolved to line/column at print time.
* Module `SYSTEM` (report §11.1, explicitly optional): the six portable entries
  of its table are lowered in place and the implementation-specific extras are
  refused by name (§9.10, `PLAN.md` W4.1).

**Out of scope (deliberate)**

* Optimisation of any kind. The C compiler does it. Our job is to emit boring,
  obviously-correct C.
* Garbage collection. `NEW` maps to `malloc` (§8.6). Oberon systems do have a
  tracing collector; the report does not promise one, and a collector is a
  project of its own. Program behaviour is unaffected; only long-running
  programs that allocate in a loop change character.
* Extensions that production Oberon compilers add beyond this report — which
  ones are worth adopting, and why most are not — are surveyed with sources in
  `PLAN.md` W8; Oberon-2 support specifically is considered there in W8.11, with
  a recommendation and the trigger that would change it. Nothing there is adopted
  until it appears in this section.
* **Oberon-2** and later dialects (`WITH`, `LOOP`, `EXIT`, type-bound
  procedures, `LEN(v, dim)`, multi-dimensional open arrays). Report §10.2 lists
  `LEN(v)` with one argument; we implement exactly that.

---

## 2. Toolchain

```toml
# crates/syntax/Cargo.toml
[build-dependencies]
lalrpop = "0.23.1"

[dependencies]
lalrpop-util = { version = "0.23.1", features = ["lexer", "unicode"] }
```

* `lalrpop` and `lalrpop-util` **must** be at the same version; LALRPOP's manual
  states that differing major/minor versions are unsupported. Pin both in one
  place (workspace `[workspace.dependencies]`).
* `build.rs` is `lalrpop::process_src().unwrap()`; the generated parser lands in
  `OUT_DIR` and is included with `lalrpop_mod!(pub oberon);`.
* We keep the **default** LR(1) construction (the lane-table algorithm). It
  accepts the grammar in `src/oberon.lalrpop` with **zero conflicts**
  **[verified]**. The alternatives are worth knowing:
  * `#[LALR]` + `LALRPOP_LANE_TABLE=disabled` → the legacy LALR(1) tables.
    Deprecated upstream; we do not use it.
  * `LALRPOP_LANE_TABLE=disabled` alone → the canonical LR(1) construction:
    identical language, much larger tables, and *materially better conflict
    diagnostics* (it prints a parse-tree-level explanation). We use it as a
    debugging switch, not in the normal build.

### Workspace layout

```
Cargo.toml                 # workspace, pins lalrpop/lalrpop-util
crates/syntax/             # lexer + grammar + syntax AST
  src/lexer.rs
  src/oberon.lalrpop
  src/ast.rs
crates/sema/               # resolver, type checker, constant evaluator
  src/sym.rs src/resolve.rs src/types.rs src/check.rs src/const_eval.rs
crates/cgen/               # C emitter + runtime header
  src/mangle.rs src/layout.rs src/emit.rs src/runtime.h
crates/obc/                # CLI driver: obc build/run/emit/check (DESIGN §9.9)
tests/corpus/              # *.Mod with expected diagnostics / expected stdout
```

The split is by *phase*, because each phase is testable on its own and the
interfaces between them are three plain data types (syntax AST → resolved AST →
C text). `crates/syntax` is the only crate that knows LALRPOP exists.

---

## 3. Pipeline

```mermaid
flowchart LR
  SRC[".Mod source"] --> LEX["Lexer<br/>hand written"]
  LEX -->|"Result&lt;(usize, Token, usize), LexicalError&gt;"| PAR["LALRPOP parser<br/>LR(1), lane table"]
  PAR --> SA["Syntax AST<br/>one node per procedure"]
  IFACE["*.sym interface files"] --> RES
  SA --> RES["Resolver<br/>scopes, imports, qualification,<br/>guard-vs-args, const eval"]
  RES --> RA["Resolved AST<br/>TypeId / SymId / offsets"]
  RA --> CHK["Checker<br/>types, read-only, exhaustiveness"]
  CHK -->|"Vec&lt;Diagnostic&gt;"| DIAG["diagnostics<br/>+ SourceMap → line:col"]
  RA --> CG["C emitter"]
  CG --> OUT[".c + .h"]
  CG --> RT["runtime.h<br/>emitted once"]
```

The parser never consults the symbol table, and the resolver never re-reads the
source text. Three facts about the report force the split, and each is a design
decision recorded in §5:

1. `a.b` is either a module-qualified name or a record field select.
2. `f(x)` is either a call with one argument or a type guard.
3. `WriteLn` is either a procedure value or the call of a parameterless
   procedure.

---

## 4. Lexer

**Decision: hand-written scanner, not LALRPOP's built-in regex lexer.**

The built-in lexer is a longest-match regex scanner with `match { ... }`
precedence blocks and regex `ignore` patterns. Two things make it unusable here:

* **Comments nest.** Report §3: "Comments … may be nested." A language
  `(* ( (* )* )*)`-balanced is not regular, and `ignore` patterns are regexes.
  The whitespace-language chapter of LALRPOP's manual reaches the same
  conclusion and falls back to a hand-written lexer.
* **`22X`, `100H` and `1..5`** would all need `match` rungs plus fixed-string
  precedence to come out right, and the result would be a regex puzzle nobody
  wants to maintain.

The scanner is 413 lines including documentation (`src/lexer.rs`
**[verified]**), emits `Result<(usize, Token<'input>, usize), LexicalError>` — the
`Spanned` shape LALRPOP expects from a custom lexer — and is plugged in with

```lalrpop
extern {
    type Location = usize;
    type Error = LexicalError;
    enum Token<'input> {
        "ident"   => Token::Ident(<&'input str>),
        "integer" => Token::Integer(<u32>),
        ...
    }
}
```

`Location` is a byte offset. Line/column is *not* tracked during lexing: a
`SourceMap` computes it from the source text when a diagnostic is printed, which
keeps three `usize`s out of every AST node.

### Lexical near-misses (all **[verified]** by `tests::lexer_near_misses`)

| Input | Tokens | Rule |
|---|---|---|
| `22X` | `CharLit(0x22)` | report §3 `string = digit {hexdigit} "X"`; `X` is not a hex digit, so it terminates the digit run |
| `100H`, `0FFH` | `Integer(256)`, `Integer(255)` | `integer = digit {hexDigit} "H"`; requires a leading digit, so `ABCH` stays an identifier |
| `1..5` | `Integer(1) DotDot Integer(5)` | `.` is only a decimal point when the next byte is not `.` |
| `1.5E3` | `Real(1500.0)` | `ScaleFactor = "E" ["+"\|"-"] digit {digit}`; `E` is consumed only if a digit follows |
| `IF` / `If` | keyword / identifier | report §3: keywords are all upper case and "cannot be used in the role of identifiers" |
| `(* a (* b *) c *)` | nothing | nested comment |
| `"A"` | `StrLit("A")` | no escape sequences in Oberon strings; a string always terminates with a NUL in our model |

Lexical errors (`IllegalCharacter`, `UnterminatedComment`, `UnterminatedString`)
carry the offending byte and are reported as `ParseError::User`; while they are
structurally parser errors, they are *not* recoverable by LALRPOP's `!`
mechanism, which is why §5 records the (deliberate) choice to fail fast in v1.

### The one dialect the lexer has: `-Odialect=lower` **[implemented; `PLAN.md` W8.10]**

`IF` / `If` above is the row this changes. A dialect that folds reserved-word case
accepts `if`, `If`, `iF` — the comparison becomes `eq_ignore_ascii_case` in one place
— while *identifiers* stay case-sensitive, so `Foo` and `foo` remain two names.

Why this is the one extension that needed a switch rather than being additive
(§13 decision 13): the report makes capital and lower-case letters distinct, so a
*report* program may legitimately write `var Begin: INTEGER` with `Begin` as a
variable, and folding would silently tokenise it as the reserved word `BEGIN` — a
program that compiles today would stop. `CASE … ELSE`, `EXIT`, a `RETURN`
statement and `MIN`/`MAX` cannot do that to an existing program; this can.

Three consequences worth stating, each with what it costs:

* **the collision has to explain itself.** A reader who wrote `var begin: INTEGER`
  would otherwise meet "unexpected `BEGIN`" with no way to see why, so the
  diagnostic carries a note naming the dialect and the reserved word it folded
  (`W8.10`), and a genuinely upper-case `BEGIN` gets no such note, because then
  nothing about the dialect explains the error;
* **the predeclared *identifiers* are not folded with the keywords.**
  `INTEGER`, `CHAR`, `TRUE`, `NIL` and the §10.2 procedure names are predeclared
  names rather than keywords, so `integer` is a *different name*, and a program may
  still declare its own. Aliasing those lower-case forms is a separate decision
  that W8.10 records as additive — it is not part of this switch;
* **the interface says so.** A non-default dialect is written into the interface
  header as `DIALECT <name>`, and a reader that does not know the name refuses the
  file rather than guessing. It is *recorded* rather than enforced: an interface is
  a list of names, those stay case-sensitive, and so a client needs no shared
  dialect to use it. A report-mode interface is byte-identical to what it was before
  this line existed.

---

## 5. Grammar

`src/oberon.lalrpop` is a direct transcription of the report's appendix, with
five changes to its productions (D1–D5) plus two decisions that shape the
grammar without changing what it accepts as a correct program (D6, D7). Each
change exists because the report's own grammar is not LR(1) at that point; each
is stated with the evidence that forced it.

One decision here is about the grammar's *shape* rather than its content, and it
predates any dialect: **a dialect is a gated grammar, not a second grammar.**
LALRPOP chooses alternatives at build time, so accepting an extension with a flag
would mean a second grammar for every flag combination — double the parser
maintenance, and two grammars that can disagree about the report's own syntax.
Instead the grammar accepts the union and the checker rejects what the selected
dialect does not allow, with a diagnostic that names the reason. That is also why
extensions whose only benefit is a different spelling of existing syntax are not
worth taking (`WITH` in `PLAN.md` W8.11): they buy familiarity and cost a
reserved word.

### D1 — `statement` is factored into assignment vs. call **[verified]**

The report's `statement = [assignment | ProcedureCall | ...]` with
`assignment = designator ":=" expression` and `ProcedureCall = designator
[ActualParameters]` puts two productions with the same prefix in one context.
Replacing `designator` by a single nonterminal and letting the lookahead decide
is the standard fix:

```lalrpop
Statement: Option<Stmt> = {
    => None,                       // report §9: a statement may be empty
    <s:AssignStmt> => Some(s),
    <s:CallStmt>   => Some(s),
    ...
};
AssignStmt: Stmt = <lo:@L> <target:Designator> ":=" <value:Expr> <hi:@R> => …;
CallStmt:   Stmt = <lo:@L> <callee:Designator> <hi:@R> => …;
```

The reduce `CallStmt → Designator •` happens only on the tokens that can follow
a complete statement (`;`, `END`, `ELSE`, `ELSIF`, `UNTIL`, `|`, `.`), never on
`:=`, so the decision is made with one token of lookahead.

Empty statements are kept, so `BEGIN ; x := 1; ; END` is legal and the empty
slots simply contribute nothing to the AST **[verified]**
(`tests::empty_statements_are_legal`).

### D2 — `( ... )` after a designator is one production, split semantically **[verified]**

The report has a *type guard* `selector = … | "(" qualident ")"` **and**
`ActualParameters = "(" [ExpList] ")"`. Both are `(` … `)` after a designator, and
the report's own Chapter 8 example puts a guard in the *middle* of a designator:

```
t(CenterNode).subnode        (report §8.1)
```

So there is no "trailing parameter list" rule to hide behind. The grammar parses
one thing:

```lalrpop
Selector: Selector = {
    "." <name:Ident> => Selector::Field(name),
    "[" <index:ExprList> "]" => Selector::Index(index),
    "^" => Selector::Deref,
    "(" <args:ExprList?> ")" => Selector::Args(args),   // guard OR arguments
};
```

and the resolver decides. The rule that makes this total:

> A paren group is a **type guard** iff it is followed by at least one more
> selector, or its single element is a bare designator that denotes a *type*.
> Otherwise it is an argument list.

This is sound because types and variables share one namespace in Oberon (report
§4: "No identifier may denote more than one object within a given scope"), so an
identifier that denotes a type can never be a value argument. Consequences to
document for users:

* `f(x)` — call (unless `x` is a type name, which cannot be an argument).
* `p(T)` — guard when `T` is a type.
* `t(T).f`, `t(T)^` — guard followed by selectors.
* `f(x)(y)` parses but is rejected by the checker ("designator applied twice").
  The report does not define it either, so a diagnosed rejection is the honest
  behaviour.

The rule is implemented in `src/resolve.rs` and executed by
`tests::reclassify_splits_guards_calls_and_module_qualification`, which checks
all four shapes (`t(T)` guard, `g(1)` call, `t(T).f` guard-then-field, `INC(x)`
predeclared call) plus `tests::type_guard_and_call_share_one_production` and
`tests::type_guard_can_be_followed_by_selectors` for the parse side, and
`tests::reclassify_leaves_no_unresolved_selector_in_the_kitchen_sink`, which
asserts that a module using every construct of the report ends up with **zero**
unresolved `Selector::Args` **[verified]**. Only two name sets are needed for
this (visible type names, import aliases) — the callee's *signature* is not,
which is what makes the pass cheap and keeps it out of the checker.

### D3 — a designator starts with *one* identifier, not with `qualident` **[verified; found by conflict]**

This is the one place where the report's grammar is genuinely ambiguous, and we
found it by building rather than by reading. `designator = qualident {selector}`
with `qualident = [ident "."] ident` and `selector = "." ident | …` means the
input `a.b` has two parses: qualifier `a` + name `b`, or name `a` + field
selector `b`. LALRPOP (canonical LR(1) construction, which explains conflicts by
showing both parse trees) said:

```
The problem arises after having observed the following symbols in the input:
  Ident
At that point, if the next token is a `"."`, then the parser can proceed in two
different ways.
First, the parser could execute the production at oberon.lalrpop:494:21, which
would consume the top 1 token(s) from the stack and produce a `Qualident`. …
Alternatively, the parser could shift the `"."` token and later use it to
construct a `QualTail`. …
```

No amount of lookahead fixes this: `a.b.c` is `M.field.field` or
`module.name.field` depending on what `a` *means*. The resolution is the same one
the report's own implementors use — decide from the symbol table:

```lalrpop
Designator: Designator = <lo:@L> <base:Ident> <sels:Selector*> <hi:@R> => …;
```

`Qualident` (with its optional `.`) survives only where nothing can follow a
designator — type names, record base types, `IS`/guard targets, case labels,
formal types, result types. In those positions `(`/`.`-shaped continuations do
not exist, so no ambiguity arises. The resolver folds a leading `Selector::Field`
into `base` when the base identifier denotes an imported module; the fold and its
negative case (a base that is *not* a module keeps its field select) are both
executed **[verified]** in the same test as D2.

### D4 — a nullable non-terminal must not appear under `*` **[verified; found by conflict]**

The first draft wrote the procedure part of a declaration sequence as

```lalrpop
ProcSection: Vec<Proc> = { => Vec::new(), <ps:ProcDeclSemi*> => ps };  // WRONG
```

LALRPOP rejected it with *"Multiple productions for the same reduction … They
could be reduced using the production on line 181 `=> Vec::new()` … or using the
production on line 182 `<ps:ProcDeclSemi*> => ps`"*: `ProcSection` then has two
derivations of ε (`ProcSection → ε` and `ProcSection → ProcDeclSemi* → ε`). This
is a general rule, not a LALRPOP quirk: `X*` over a nullable `X` is ambiguous.
The fix is to let the star own the empty case:

```lalrpop
ProcSection: Vec<Proc> = <ps:ProcDeclSemi*> => ps;
```

The same reasoning is why `ConstSection`/`TypeSection`/`VarSection` are written
as `{ => Vec::new(), "CONST" ConstDeclSemi* => … }` — there the ε is a separate
production with a *non-nullable* right-hand side (it starts with the section
keyword), so no duplicate derivation exists.

### D5 — helper non-terminals for repetition

`IdentList`, `Qualident`, `ExprList`, `FieldLists`, `CaseLabels`, `SetElems`,
`StatementSequence` exist only so the star/tail idioms read uniformly; they have
no counterpart in the report and none in the AST.

### D6 — `RETURN` is a statement, and every return leaves by one door **[revised in W8.4]**

Report §9's `statement` list does **not** contain `RETURN`; §10 puts it at the
end of `ProcedureBody`:

```
ProcedureBody = DeclarationSequence [BEGIN StatementSequence] [RETURN expression] END.
```

The first transcription of this design took that literally, and described what it
bought: **every procedure body has a single exit point**. What it cost was real,
though — a procedure could not return from inside a `CASE` — and the guarantee was
the *only* thing the literal reading was for. So `RETURN [Expr]` is a statement now
(PLAN.md W8.4), and the guarantee is kept by construction instead: a procedure with
a result has one accumulator (`ret__acc`, zero-initialised so that a path which does
not return yields a defined value) and one exit (`ret: return ret__acc;`), and every
`RETURN` is `ret__acc = …; goto ret;`. A proper procedure's `RETURN` is the same
jump to a bare `return`. The report's tail clause is therefore gone from the
grammar, and with it the one place the two forms could have been told apart by
lookahead.

The check relaxes the same way: a function must *contain* a `RETURN` with a value
rather than end with one, a proper procedure may not return a value, and a naked
`RETURN` in a function is an error. The single-*exit* property is unchanged; what
changed is that it is the emitter's doing rather than the grammar's.

### D7 — error handling and recovery **[implemented; limits found by conflict]**

Two properties are wanted, and they are independent:

1. **Every error is located.** A diagnostic carries a byte span, and the
   rendering (§10) turns it into `file:line:col`, echoes the source line, puts a
   caret under the span, and adds a note when the rule is not obvious. Message
   text is not pre-formatted in the parser or the checker: they produce
   `Diagnostic` values, and one function renders them, so syntax and semantic
   errors look alike.
2. **One run reports every error it can.** LALRPOP's `!` token plus

   ```lalrpop
   grammar<'input, 'err>(errors: &'err mut Vec<ErrorRecovery<usize, Token<'input>, LexicalError>>);
   ```

   resynchronises, and the recovery is *recorded* rather than swallowed, so the
   driver reports the syntax errors and the semantic errors found in the same
   run **[verified by `diag_tests` and by seven `e2e` checks]**.

Recovery is enabled in exactly one place — **the statement sequence**:

```lalrpop
Statement: Option<Stmt> = {
    => None,
    <s:AssignStmt> => Some(s),
    …
    <e:!> => { errors.push(e); None },
};
```

A statement the parser cannot accept becomes an empty one, the parser skips
input to the next `;`, and the rest of the body is still parsed. That is the
recovery that pays, because statements are where syntax errors cluster and where
recovery is cheap: the skipped tokens are the broken statement.

**Recovery in declaration sections is not attempted, and the reason is a
measured one.** Four shapes were tried — recovery on the `;` that ends a
declaration, recovery on the next section keyword, a nullable item under a
non-nullable list, and a `!` alternative inside the list rule — and LALRPOP
rejects all four with a *local ambiguity on the error token*: because the report
lets each declaration section be empty, the section's ε production and the error
token compete (end the section, or swallow the error?), and LALRPOP's LALR(1)
construction adds the error token to every lookahead set, so the ε-reduce is a
live alternative wherever the error can be shifted. The statement case escapes
this because `StatementSequence` is non-nullable and its single `!` sits in the
item rule — the same shape applied to declarations does not, because the sections
*are* nullable.

The consequence is bounded and acceptable: a declaration-level or module-level
syntax error is reported **once**, located and explained, and the file stops
there (§3's build order means the driver has nothing to emit anyway). Recovery
inside a `ProcedureBody` is capped the same way: a broken procedure declaration
is reported and the file stops, rather than resynchronising inside a body whose
declarations are then unusable.

Two entry points exist, deliberately:

* `parse_module` fails fast (used where a module is *required* to be well formed,
  such as reading a `.sym` interface), and
* `parse_with_diagnostics` / `check_module_recovering` recover.

A *fatal* error — a missing `END`, an unterminated comment, an illegal character
— is not absorbed by any recovery point, but it is still converted to a located
diagnostic rather than printed as a `ParseError` **[verified]**, so a reader
never sees a byte offset.

### D8 — the `CASE` default arm is spelled `ELSE S`, not `| S` **[decided; `ELSE` added in W8.1]**

The report's `case = [CaseLabelList ":" StatementSequence]` puts the label list and
its colon in one optional group, so a *default* arm is written as a bare statement
sequence: `| S`. Our grammar parses `CaseArm = labels? body?` with
`body = ":" StatementSequence` — both optional, independently — so the default arm
is spelled `| : S` (an empty label list followed by the colon), and the arm `|` on
its own is the default with no statements. The report's bare `| S` is therefore
*rejected*, with a diagnostic that names what was expected:
`error: unexpected `:=` … = note: expected END, ., .., ,, : or |`, pointing at the
missing colon.

The report's bare `| S` is still rejected, and not because it was forgotten: a
statement sequence and a label list share a first set (a call statement starts with
the same qualident a constant label may), so telling `| k := 2` from `| k: …` needs
two tokens of lookahead that LALR(1) does not have and the reference compiler's
recursive descent does. `ELSE` is the answer instead (PLAN.md W8.1): it is already
a report keyword, its arm is the same node as a label-less `|` arm — which the
checker and the emitter have always treated as the default — and two defaults in one
`CASE` are refused by row 63 rather than silently ordered. The empty-label spelling
`| : S` is still accepted, so a program that already used it keeps working; the
deviation from the report is that the default arm has a spelling its grammar does
not, which `ELSE` supplies.

---

## 6. AST

**Design rule: the tree is organised per procedure.** There is no flat
declaration list with back-references and no arena of anonymous nodes. A `Proc`
owns everything that belongs to it:

```rust
pub struct Module<'input> { name, imports, decls: Decls<'input>, body: Vec<Stmt<'input>>, … }

pub struct Decls<'input> {           // exactly the report's DeclarationSequence order
    pub consts: Vec<ConstDecl<'input>>,
    pub types:  Vec<TypeDecl<'input>>,
    pub vars:   Vec<VarDecl<'input>>,
    pub procs:  Vec<Proc<'input>>,   // <- nested procedures, recursively
}

pub struct Proc<'input> {
    pub name: Ident<'input>, pub export: bool,
    pub params: Vec<Param<'input>>,  // one entry per FPSection
    pub result: Option<QIdent<'input>>,
    pub decls: Decls<'input>,        // its own constants/types/variables/procedures
    pub body: Vec<Stmt<'input>>,
    pub ret: Option<Expr<'input>>,   // the single trailing RETURN, if any
    pub end_name: Ident<'input>,     // repeated after END; must equal `name`
    pub span: Span,
}
```

Why this shape:

* The checker and the emitter both want to work "one procedure at a time": push
  a scope, declare parameters, walk the body, pop. Nesting is the tree, so
  **nothing is ever looked up by procedure name across a side table**.
* `end_name` is kept, not checked at parse time, because "end identifier must
  repeat the procedure identifier" is a report §10 rule with a *span* and a
  *message*, and belongs to the resolver.
* Span is `Span { lo: u32, hi: u32 }` — byte offsets, 4 GiB of source is enough
  (Oberon modules are kilobytes), and `u32` keeps the node small.
  `@L`/`@R` bind `usize`, which the actions narrow once.
* Every leaf takes its span from `@L`/`@R`; composite expressions derive theirs
  from their children (`Expr::binary` joins the two operand spans), so spans cost
  two `usize` writes per node and no bookkeeping.

**Lifetimes.** The prototype stores `String` for identifiers and string literals
(the mechanical form, proven end to end). Production should use `&'input str`
slices into the source text: the compiler handles one module at a time, the
source outlives every AST node and diagnostic, and this removes one allocation
per identifier occurrence. If profiling ever shows repeated string comparison in
the symbol table to be hot, intern to a `Symbol(u32)` — but that is a later,
purely local change, since no phase compares identifiers across the AST/resolved
boundary.

**Syntax AST vs. resolved AST.** The parser produces the syntax AST above, and
that stays the only tree: the semantic pass is *single walk* (a scope chain with
types built on the fly, like the reference compiler), so there is no second,
annotated tree. The few types the emitter will need — the `BYTE`/`INTEGER` and
string cases, open arrays, guards, type-`CASE` narrowings — come from a side
table keyed by span, which is the cheap equivalent of an annotation because
spans are unique. A second tree would be tidier to consume and twice the code;
the table below is what it would contain, and doubles as the description of the
rewrites the resolver performs in place:

| field | syntax AST | resolved AST |
|---|---|---|
| type references | `TypeExpr::Named(["M","T"])` | `TypeId` into the type interner |
| designator base | `["t"]` + selectors | `SymId` + constant offset chain |
| paren group | `Selector::Args(Option<Vec<Expr>>)` | `Selector::Call(SymId, Vec<Expr>)` or `Selector::Guard(TypeId)` |
| `ExprKind::Int(u32)` in a type/label context | literal | folded constant |
| everything else | identical | identical |

Keeping two types is deliberate: phases that only re-annotate cannot silently
change the tree, and the emitter can be written against a "nothing can fail"
interface. The first three rows of that table are the *only* rewrites the
resolver performs that change a node's shape, and they are all implemented in
`src/resolve.rs`; the remaining work is attaching `TypeId`/`SymId`, which
is annotation, not restructuring.

---

## 7. Resolver: names, scopes, imports **[implemented in `src/sema.rs`]**

Implemented, with the row numbers of §8.5 in each check's comments: the scope
chain and the declaration order, type resolution with the report's single
forward-reference rule, module-qualified lookup through interface files, the
`FOR` lock, the read-only rules and the import checks. Verified by
`check_tests::report_out_module_checks_clean` (the report's own `Out`, imports
and all, produces **no** diagnostics) and
`check_tests::kitchen_sink_checks_clean` (one module using every construct of
the report likewise).

### Scopes

One `Scope` per module and per procedure, with a parent link; the scope chain is
walked outward on lookup, exactly as report §4 defines visibility ("from the
point of the declaration to the end of the block to which the declaration
belongs").

Per scope, **two passes over the declaration part**:

1. *Declare*: insert every constant, type, variable, and procedure name with its
   kind. This is what makes `Tree = POINTER TO Node; Node = RECORD … END`
   legal (report §6.4: "the identifier `T` can be declared textually following
   the declaration of `P`"). Insertion reports the §4 duplicate-name error.
2. *Resolve*: type each declaration, check constant expressions, then walk
   procedure bodies. Because pass 1 completed for the whole scope, a pointer base
   never needs a second resolution round.

Forward references outside pointer bases are *not* allowed (report §4), and the
resolver enforces that by resolving in textual order: a type declaration that
names a not-yet-resolved *non-pointer* type in the same scope is an error. Since
all names are visible after pass 1, we track a per-scope "resolved up to"
watermark for this check rather than relying on the symbol table's contents.

### Imports

`IMPORT Texts, Oberon;` and `IMPORT M := Other;` (report §11) insert the alias as
a *module* symbol. A qualified reference `M.x` is resolved in two steps now that
qualification is semantic (D3): the resolver rewrites a leading
`Selector::Field(x)` into `base = ["M","x"]` when `M` denotes a module, then
looks `x` up in that module's interface and requires the export mark.

**An interface must name the modules its own types refer to.** This was not in
the first draft of the format, and its absence is a real hole: `Oberon`'s
interface says `VAR Log: Texts.Writer`, and a client that imports `Oberon` but
not `Texts` must still resolve that type. The format therefore carries an
`IMPORT` line, and the reader resolves qualified names in the *defining*
module's import list rather than in the importer's scope. Found by trying to
check the report's own `Out`; the alternative — rendering every imported type
structurally, as ETH's `.sym` files do — keeps interfaces self-contained but has
to serialise recursive structures, a much larger format.

Interface files (`<Module>.sym`) are text, one declaration per line, written by
the emitter companion and read by the resolver:

```
MODULE Out
VERSION 1
IMPORT Texts, Oberon
PROC Write(ch: CHAR)
PROC WriteInt(x: INTEGER; n: INTEGER)
PROC WriteLn
VAR W: Texts.Writer            (* exported variables are read-only *)
CONST N = 100
TYPE Node = RECORD key: INTEGER; left, right: Tree END
```

Text rather than a packed binary blob: it is diffable, greppable and versionable,
and parsing it is trivial next to the compiler that writes it. The *rendered
types* on those lines are ordinary Oberon type syntax, so the reader parses them
with the very production the module parser uses (`pub RenderedType` in
`oberon.lalrpop`) instead of growing a second parser for its own output — which
also means an interface cannot drift into a syntax the compiler would not
accept.

Three details make the format unambiguous, and all three are exercised by the
prototype codec in `src/sym.rs` **[verified]**:

* a parameterless proper procedure is written `PROC WriteLn` with no
  parentheses at all, matching how the report lets it be called (report §9.2);
* a formal parameter's type is `{ARRAY OF} qualident` (report §10.1), so a
  parameter spec can never contain parentheses — the codec *rejects* an inline
  `PROCEDURE (…)` parameter type, which is what keeps the closing `)` of the
  parameter list unambiguous;
* render/parse is a round trip and render is a fixed point on the parsed result,
  so a written interface and a re-read interface cannot drift.

The codec prototypes only the *format*; resolving the parsed declarations into
the scope chain is the resolver's job.

### Read-only entities

Report §9.1 and §10.1 forbid assigning to: constants, imported variables,
value parameters of structured type, and the control variable of a `FOR` inside
its body. All four are the same check — `is_assignable(designator)` — applied at
one place in the assignment checker, with a message that names the reason.

---

## 8. Semantics

### 8.1 Type representation **[implemented in `src/types.rs`]**

Types are interned into a `TypeId` so that equality is `u32` comparison for the
structural kinds, and the report's *nominal* rules are implemented explicitly.
All of it is executed by `src/types.rs` **[verified]**, which is
deliberately AST-free — it takes `TypeId`s, so the checker, the folder and the
back end's layout pass share one implementation of the rules:

| kind | equality | extension |
|---|---|---|
| `INTEGER`, `BYTE`, `REAL`, `BOOLEAN`, `CHAR`, `SET` | by name | — (report §6.1: `BYTE` and `INTEGER` are compatible both ways) |
| `ARRAY len elem` | **structural**: same length *and* equal element type | — |
| `RECORD` | **nominal**: identical declaration | `T` extends `T0` if `T == T0` or `T`'s base extends `T0` (report §6.3) |
| `POINTER TO T` | **nominal** | inherits the base types' extension relation (report §6.4) |
| `PROCEDURE(params): result` | **structural**: identical parameter types/modes and result | — |
| `ARRAY OF T` (open) | only as a formal type | — |
| `NIL`, string literal | internal singleton types | — |
| `ErrorType` | poison, compatible with everything | — |

`ErrorType` exists so that one bad expression does not produce a cascade of
follow-on errors; every check returns it after reporting, which is why every
predicate in `types.rs` short-circuits on it.

The rules that are easy to get subtly wrong all have a test in `types.rs`, named
after the report section they come from: pointer identity *and* extension follow
the base (§6.4); record extension is reflexive, transitive and one-way (§6.3);
a field lookup walks the base chain, most-derived first (§6.3); the four
assignment exceptions of §9.1; the parameter-mode rules of §10.1, including "a
record formal may be a base type of the actual's" but "`VAR` needs identical, not
merely compatible"; the operand rules of §8.2.1-§8.2.4, including that the set
operators are spelled like the arithmetic ones; and the signatures of §10.2.

### 8.2 Assignment and parameter compatibility

The report §9.1 states the rule plus four exceptions, and §10.1 states the
parameter rule. Both are implemented as one `is_assignable(dst, src)` predicate
with a `why` output:

| case | allowed when |
|---|---|
| same type | always |
| `BYTE` ↔ `INTEGER` | both directions (report §6.1) |
| `NIL` → pointer or procedure type | always (report §9.1.1) |
| string → `ARRAY n OF CHAR` | `len(string) < n`; a NUL is appended (report §9.1.2) |
| 1-char string / `CHAR` | single-character string to `CHAR` |
| record → record, pointer → pointer | source type extends destination type (report §9.1.3; §6.4 gives pointer types the same relation through their base types) |
| open array → array of equal base type | equal element types (report §9.1.4) |
| VAR parameter | identical types, except formal record type may be a *base type* of the actual's (report §10.1) |
| value parameter of structured type | as assignment, and the formal is read-only in the callee |

Two consequences of the table are easy to get wrong in the back end, and both
are settled here:

* A string literal assigned to `ARRAY n OF CHAR` is *length-checked* (check 9)
  and zero-padded, so the result is NUL-terminated and `LEN`-consistent.
* An *array-to-array* `CHAR` assignment of equal length is a plain bounded copy:
  if the source has no NUL inside its length, neither does the destination.
  Oberon strings are NUL-terminated **by construction**, not by type, so the
  checker cannot rule this out — therefore the relational operators must not
  assume termination either, and they do not (see §9.5's `rt_charcmp`).

There is **no implicit `INTEGER`↔`REAL` conversion**: `x + i` with `x: REAL` and
`i: INTEGER` is an error, with the diagnostic suggesting `FLT(i)` (report §8.2.2
requires both operands of `+ - * /` to be of the same type). This is the check
most likely to annoy users coming from C, and the message says exactly what the
report says.

### 8.3 Constant evaluation **[implemented in `src/const_eval.rs`]**

`const_eval` folds over the *syntax* AST with a small value domain
(`Int(i32) | Real(f32) | Bool | Char | Str | Set(u32)`), and is used for exactly
four contexts: constant declarations (§5), array lengths (§6.2), `CASE` labels
(§9.5), and the `BY` increment of `FOR` (§9.8). Operands that reach it must be
constants (report §5: "can be evaluated by a mere textual scan without actually
executing the program"), so any designator must resolve to a constant; `LEN` of a
fixed-size array is folded by the checker, which knows lengths, and the folder
says so rather than guessing.

The folder implements the *same* arithmetic as the emitted C — floor `DIV`/`MOD`
including the `y == -1` case, the shifts *as the report defines them* (`x * 2^n` and
`x DIV 2^n`, the rotation mod 32 — R22), `{a .. b}` as
`((1u << (b-a+1)) - 1u) << a`, set union/difference/intersection/symmetric
difference, and `INTEGER` arithmetic that wraps as `-fwrapv` does — and
`tests::agrees_with_the_emitted_c` checks that against a table of values produced
by the C side from its own runtime helpers **[verified]**. That test is the point
of the section: a folder that disagrees with the running program is a bug class
with no other detector.

Two consequences of the report's expression grammar that the folder pinned down,
and which the emitter inherits:

* `SimpleExpression = ["+"|"-"] term {AddOperator term}`, so a leading sign
  applies to the whole term: **`-7 DIV 3` is `-(7 DIV 3)` = `-2`**, not
  `(-7) DIV 3 = -3`. Parentheses are required for the latter.
* A single-character string is a `CHAR` value (§9.1.2), which is what makes the
  report's own `ORD("0")` (§10.1) work.

### 8.4 Predeclared environment

Inserted into the outermost scope before anything else, as report §10.2 defines:

| name | signature | note |
|---|---|---|
| `ABS(x)` | numeric → same type | |
| `ODD(x)` | INTEGER → BOOLEAN | |
| `LEN(v)` | array → INTEGER | |
| `LSL`, `ASR`, `ROR` | (INTEGER, INTEGER) → INTEGER | `x * 2^n`, `x DIV 2^n`, rotate right by n (report §8.2.2); a negative count is undefined and is masked |
| `FLOOR(x)` | REAL → INTEGER | round down |
| `FLT(x)` | INTEGER → REAL | |
| `ORD(x)` | CHAR/BOOLEAN/SET → INTEGER | |
| `CHR(x)` | INTEGER → CHAR | |
| `INC`, `DEC` | (VAR INTEGER [, INTEGER]) | desugars to assignment |
| `INCL`, `EXCL` | (VAR SET, INTEGER) | set union / difference |
| `NEW(v)` | (VAR pointer) | allocation, §8.6 |
| `ASSERT(b)` | BOOLEAN | traps if false |
| `PACK`, `UNPK` | (VAR REAL, VAR INTEGER) | exponent packing |

`LEN` needs care: for a fixed-size array it folds to the declared length; for an
open array parameter it becomes the hidden length parameter (§8.7).

Report §11.1's `SYSTEM` is *not* in this table: it is a module, imported like one
(§11), and its entries are dispatched by name in the checker — `ADR`, `GET` and
`PUT` take a variable and `SIZE` takes a type, so there are no five signatures to
declare here (§9.10, `PLAN.md` W4.1).

### 8.5 Type and semantic checks — catalogue

Each row is one diagnostic with one span. `E` = error, `W` = warning.

| # | check | source | kind |
|---|---|---|---|
| 1 | identifier used but not declared, and not predefined | §4 | E |
| 2 | identifier declared twice in one scope | §4 | E |
| 2b | a declaration in an *inner* scope hides the outer one instead — the universe of predefined identifiers is the outermost scope (§10.2), so a module or procedure may declare `ABS` and have it hide the predefined one | §4 | — |
| 3 | qualifying module not in the import list | §11 | E |
| 4 | object referenced through a module qualifier is not exported | §11 | E |
| 5 | import alias declared twice / module imports itself | §11 | E |
| 6 | assignment to a read-only entity (constant, imported variable, structured value parameter, `FOR` control variable in its body) | §9.1, §10.1 | E |
| 7 | assignment type mismatch (table in §8.2) | §9.1 | E |
| 8 | `NIL` assigned to something that is not a pointer/procedure | §9.1.1 | E |
| 9 | string too long for the destination array | §9.1.2 | E |
| 10 | array index count ≠ array dimension count | §8.1 | E |
| 11 | index expression not `INTEGER` | §8.1 | E |
| 12 | `^` applied to a non-pointer | §8.1 | E |
| 13 | field selection on a non-record (or non-pointer-to-record) | §8.1 | E |
| 14 | unknown field; field is private and the access is from another module | §6.3 | E |
| 15 | type guard `v(T0)` where `T0` is not an extension of `v`'s declared type | §8.1 | E |
| 16 | type guard on something that is not a pointer or a VAR record parameter | §8.1 | E |
| 17 | `IS` with the same two conditions | §8.2.4 | E |
| 18 | arithmetic operand not numeric, or the two operands differ in type | §8.2.2 | E |
| 19 | `DIV`/`MOD` operand not `INTEGER` | §8.2.2 | E |
| 20 | logical operator operand not `BOOLEAN` | §8.2.1 | E |
| 21 | set operator operand not `SET` | §8.2.3 | E |
| 22 | relation not defined for the operand types | §8.2.4 | E |
| 23 | `x IN s`: `x` not `INTEGER`, or `s` not `SET` | §8.2.4 | E |
| 24 | `IF`/`WHILE`/`UNTIL` guard not `BOOLEAN` | §9.4, §9.6, §9.7 | E |
| 25 | call: wrong number of actuals | §9.2 | E |
| 26 | call: actual not compatible with its formal (table in §8.2) | §10.1 | E |
| 27 | VAR actual is not a designator denoting a variable | §9.2, §10.1 | E |
| 28 | calling something that is not a procedure (or calling a procedure value that is not of procedure type) | §9.2 | E |
| 29 | designator applied twice (`f(x)(y)`) | — (D2) | E |
| 30 | bare designator statement whose object is not a parameterless procedure | §9.2 | E |
| 31 | function procedure used as a statement without a call, or a proper procedure used in an expression | §9, §10 | E |
| 32 | `FOR` control variable is not an `INTEGER` variable, is read-only, or is assigned inside the body | §9.8 | E |
| 33 | `FOR` bounds not `INTEGER` | §9.8 | E |
| 34 | `BY` increment not a constant, or equal to zero, or not an integer constant | §9.8 | E |

**Status.** Rows 1-5 are the resolver's and 6-60 the checker's; both are
implemented in `src/sema.rs` and driven by
`check_tests::catalogue_rows_report_what_they_should`, which has a case per row
(each naming the row number, so a failure points at the rule; rows with two
shapes have two cases). **Every row is checked**: field visibility across modules
(rows 4 and 14's private-field halves) follows the field's own `*`, which travels
through the interface, so naming an unmarked field of an imported record is an
error while a module's own unmarked fields stay private to it. Row 50 holds on
both sides now:
assigning a *local* procedure to a procedure variable is an error, and so is
passing one as an actual. Row 54's unused-*procedure* half is checked too: a procedure this
unit declares and never uses is a warning, with the use recorded where a
designator resolves to a procedure (so a call, a use as a value and an exported
procedure all count), and a self-recursive one counting as used, which is the
one imprecision.

Rows 1-5 are the resolver's; 6-60 are the checker's. The two *predicates* they
are built from — the type algebra above and the constant folder below — are
already implemented and tested, so the checker's remaining work is bookkeeping
around them rather than new rule invention. Rows 55-60 are report §11.1's module
and belong to the escape hatch's item, `PLAN.md` W4.1: the argument rules are
`check_tests::catalogue_rows_report_what_they_should`'s cases, the by-name
refusals and the located messages are `e2e`'s `system-*` rows, and the "nothing
is added or removed by `-Ochecks`" half of row 60 is `csmoke`'s
`system-*` assertions plus `e2e`'s `system-emits-the-same-c-under--Ochecks=n`.
| 35 | `CASE` label type does not match the case expression's type | §9.5 | E |
| 36 | duplicate `CASE` label, or overlapping ranges | §9.5 | E |
| 37 | type-`CASE` label that does not extend the case variable's type | §9.5 | E |
| 38 | type-`CASE` on something other than a pointer or VAR record parameter | §9.5 | E |
| 39 | type-`CASE` where the arms are not exhaustive and there is no default arm | §9.5 | W |
| 40 | constant expression expected (const decl, array length, case label, `BY`) but the expression is not constant | §5, §6.2, §9.5, §9.8 | E |
| 41 | array length not a positive constant | §6.2 | E |
| 42 | record base type (`RECORD (T)`) is not a record, or is a self/cyclic extension | §6.3 | E |
| 43 | field declared twice inside one record | §6.3 | E |
| 44 | pointer base type is not a record | §6.4 | E |
| 45 | procedure type/declaration with a record or array result type | §10.1 | E |
| 46 | `RETURN` with a value in a proper procedure, a naked `RETURN` in a function, or a function whose body contains no `RETURN` at all (W8.4) | §10 | E |
| 64 | `EXIT` outside any loop, which has no loop to leave (W8.5) | W8.5 | E |
| 47 | procedure end identifier ≠ procedure name | §10 | E |
| 48 | export mark outside module scope, or on a formal parameter | §4 | E |
| 49 | procedure without parameters declared as `PROCEDURE F()` but called as `F(x)` (and vice versa) | §10.1 | E |
| 50 | local procedure assigned to / passed as a procedure variable | §6.5 | E |
| 51 | predeclared procedure used as a value | §6.5, §10.1 | E |
| 52 | `SET` element constant outside 0..31 | §10.2 | E |
| 53 | `SYSTEM.<entry>` where the entry is not one the report's §11.1 table defines — its RISC-specific extras (`VAL`, `ADC`, `SBC`, `UML`, `COND`, `LED`) included — refused *by name*, never silently | §11.1 | E |
| 54 | unused import / unused local procedure | — | W |
| 55 | `SYSTEM.SIZE`'s argument is not a type name | §11.1 | E |
| 56 | `SYSTEM.ADR`'s argument is not a designator whose address exists (a constant, a structured value parameter, a procedure) | §11.1 | E |
| 57 | `SYSTEM.GET`'s variable, or `SYSTEM.PUT`'s value, is not of a basic type | §11.1 | E |
| 58 | `SYSTEM.GET`'s variable is not writable — row 6's reasons, since `GET` writes it | §11.1, §9.1 | E |
| 59 | a `SYSTEM` entry used with the wrong number of arguments (the entries have no formal parameter list for row 25 to check) | §11.1 | E |
| 60 | not a check but a rule the lowering states: `SYSTEM.COPY`'s count is in *words* and `SIZE(T)` is the byte size including padding (§9.10), and no `-Ochecks` setting adds or removes anything for `SYSTEM` | §11.1 | — |
| 61 | `MIN`/`MAX`'s argument is not a *type name* | W8.2 | E |
| 62 | `MIN`/`MAX` of a type that has no extreme — `REAL`, `SET`, `BOOLEAN`, a structured type: named, rather than given a zero | W8.2 | E |
| 63 | a `CASE` with two default arms (`ELSE` and a label-less `\|` arm) | W8.1, §9.5 | E |

### 8.6 Storage and allocation

`NEW(p)` on `p: P` where `P = POINTER TO T` (report §6.4) emits a heap block of
`sizeof(struct T)` **plus a descriptor pointer in front of it**, stores the
descriptor of the *static* type `T`, and assigns the payload address to `p`.
Failure of `malloc` yields `NIL`, which is exactly the report's specified
behaviour ("Failure of allocation results in `p` obtaining the value `NIL`").
There is no collector (see §1).

### 8.7 What the resolver hands the checker

The resolver's `Selector::Guard`/`Selector::Call` split, the folded constants,
and the `TypeId` on every expression mean the checker is a pure bottom-up walk
with no scope lookups of its own except for the read-only rules, and the emitter
can assume every node is well formed. This is the only place where phases are
entangled, and it is worth it: the alternative is that the C emitter repeats the
symbol table walk, which is where compilers grow their second, subtly different
type checker.

---

## 9. C back end

The back end is a syntax-directed emitter over the *resolved* AST: one C function
per `Proc` node in pre-order, one file-scope declaration set per module, one
`void <Module>__init(void)` for the module body. It performs no optimisation and
no reordering beyond what the C compiler requires.

### 9.1 Compilation model **[implemented in `src/cgen.rs`]**

```
cc -std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror \
   -Wno-overflow Foo.c Bar.c oberon_runtime.c -o program
```

`-fno-strict-aliasing` is required for the same kind of reason as `-fwrapv`: a
type `CASE` and a type guard view a record through a derived type
(`(Derived *)base`), and C's common-initial-sequence guarantee covers unions, not
a base struct as the first member. The alternative — giving up the `.base.`
layout — would cost every up-cast and every inherited field access an offset
computation, so the flag is the cheaper honesty.

`-Wno-overflow` is the same argument one level up: gcc reports the *overflow* of
an expression it can fold, including one it folds symbolically —
`(MAX - m2) + (MAX + m2)` is reported as "integer overflow in expression … results
in -2" with `m2` a variable — but with `-fwrapv` that wrapping is exactly the
behaviour the line above has chosen. A *constant* subexpression is still folded by
the front end before C ever sees it (the folder, §8.3), so the warning is silenced
for the cases where the language's answer is the value we want, and the whole
generated corpus (`fuzz`, `e2e`) is what checks the values.
**[verified: the generator's seed 30 is the case that settled it — no value fold
can reach a symbolic cancellation.]**

`-fwrapv` is required, not cosmetic: Oberon's `INTEGER` is a 32-bit type whose
overflow behaviour the report leaves unspecified, and one of the two behaviours
we can choose from (C's undefined signed overflow, or two's-complement wrap) must
be chosen. Wrapping is what every Oberon implementation does and what
`INC(x)` around the largest `INTEGER` implies, so we pin it **[verified: the whole
`csmoke` suite is built with `-fwrapv`]**.

### 9.2 Type mapping **[verified; `csmoke` executes every row, `e2e` compiles and runs programs that use them]**

| Oberon | C |
|---|---|
| `INTEGER` | `int32_t` |
| `BYTE` | `uint8_t` |
| `REAL` | `float` |
| `BOOLEAN` | `int32_t` with values 0/1 (so `ORD(b)` is the C value) |
| `CHAR` | `char` (8-bit, plus `ORT`-style casts at the edges) |
| `SET` | `uint32_t` |
| `ARRAY n OF T` | `T name[n]` (inside a `struct` when it must be passed by value) |
| `RECORD … END` | `struct <Module>__<Name>` |
| `POINTER TO T` | `struct <Module>__<Name> *` (payload pointer; descriptor lives in the block header) |
| `PROCEDURE (…)` | function pointer `R (*name)(void *sl, …)` |
| `NIL` | `NULL` |
| `ARRAY OF T` (open formal) | `T *name, int32_t name__len` |

### 9.3 Records, extension, and type descriptors **[verified; `csmoke` for the layout and `rt_is`/`rt_rec_copy`, `e2e` for `IS`, guards and a type-`CASE` on a three-level extension, `golden` for the emitted shapes]**

Report §6.3/§6.4 make records extensible and pointers type-tested, so the layout
must support `IS`, guards, and `CASE` on a record/pointer variable:

```c
typedef struct ObDesc ObDesc;
struct ObDesc { const ObDesc *base; int32_t id; const char *name; };

typedef struct R  R;
typedef struct R0 R0;                 /* R0 extends R */
struct R  { const ObDesc *desc; int32_t x; };   /* the base-most record only */
struct R0 { R base; int32_t y; };               /* base is the FIRST member */
```

Rules:

* An extension's struct contains the base struct as its **first member**, and
  **only the base-most record of an extension chain declares the `desc` field**.
  The descriptor is therefore at offset 0 of every view of the object, so
  `*(ObDesc **)p` is the same word whether `p` is a `Base *`, a `Derived *` or the
  address of a record variable — which is exactly what `IS`, a guard and a type
  `CASE` read.
* Every record **variable** carries its static type's descriptor in that word,
  and so does a `NEW`-allocated object: the descriptor sits *inside* the record,
  not in a header in front of it, so `sizeof` a record is its Oberon size plus
  one pointer.
* Copying a record **preserves the destination's descriptor** (the emitter calls
  `rt_rec_copy`, never a bare C struct assignment): an up-cast `b := d` leaves `b`
  with its own static type, so a base variable can never advertise a derived
  dynamic type.
* `v IS T` and `v(T)` walk the `base` chain: `rt_is(desc(v), &TD_T)`
  **[verified in `csmoke`]**, and a failed guard calls `rt_trap` (report §8.1:
  the guard "aborts program execution, if it is not of type T0").
* A `CASE` over a record/pointer becomes `switch (RT_DESC(v)->id)` with one
  `case TD_Ti:` per label and an arm-local narrowing cast `(Ti *)v`
  (report §9.5 gives the case variable the label's type inside the arm). This
  needs no runtime helper and compiles to a jump table **[verified]** by the
  `typecase-id-*` cases: only the arm whose `id` matches runs, the cast reads
  the derived type's field correctly, and an `id` that is not listed falls
  through to the trapping `default`.

### 9.4 Memory and traps

```c
void *rt_new(int32_t size, ObDesc *d);   /* malloc header+payload; NIL on failure */
void  rt_trap(const char *);             /* bounds, NIL, case, ASSERT */
```

**The address model** (report §11.1's `SYSTEM`, §9.10): the report's `mem[a]` is
an array of *words*, and this implementation defines an address as a **byte**
address held in an `INTEGER` — so `ADR(v)` is `&v` and `COPY`'s `n` counts words,
`n * 4` bytes. The one expression that is not an `INTEGER` is an `ADR` call
itself, which is as wide as the host's pointers (R31).

**Every memory access, and why it is safe.** The design's promise is that a
memory access in emitted code is either provably in bounds by the type system or
guarded by something visible in the emitted C — with `SYSTEM` (§9.10) as the only
deliberate escape. This table is that promise, audited access by access
(PLAN.md W7.3); the last column names the test that executes the row.

| access | safe because | test |
|---|---|---|
| array index, fixed array | `rt_index(i, n, at)` — traps with the offset | `e2e/Trap.mod` (a constant index out of range), `diag_tests::a_constant_index_out_of_range_warns_and_still_traps` (the warning *and* five emitted checks) |
| array index, open-array parameter | `rt_index(i, name__len, at)` against the hidden length the caller passed | `e2e`'s `trap-index-in-open-array` (added in W7.4; before it, this row was *false*: an open-array index was emitted with no check at all, and the parameter was dereferenced as if it were a pointer to the array) |
| record field | the layout is computed by the emitter, so every field offset is in bounds by construction | `csmoke` (descriptors and layouts), `golden` (the emitted shapes) |
| pointer dereference (`p.f`, `p^`) | `rt_nil(p, at)` — traps with the offset | `e2e`'s `nil-field-dereference-traps` and `nil-caret-dereference-traps`, `fuzz`'s adversarial mode (a dereference of a zeroed pointer variable), `emit_tests::a_pointer_access_is_checked_for_nil`. **Exception**: when the operand has a side effect (an index expression containing a call) the check is *not* emitted, because `rt_nil` reads it twice (R26) |
| failed type guard `v(T)` | `rt_guard_is` — traps | `e2e`'s `failed-guard-traps` (pointer spelling) and `fuzz`'s adversarial mode (a guard asking for the extension while the object is the base); the record spelling is R15 |
| type-`CASE` with no matching label | `rt_trap_pos("CASE: no matching type", subject)` — traps with the offset | `csmoke` (the descriptor-id switch), `e2e`'s `trap-case-no-match` (which asserts the exact line) |
| whole-array and whole-record copy | `rt_rec_copy` / `rt_strcopy` / `memcpy` with the length the types give | `golden` (R14's exact-length string case), `csmoke` |
| string literal initialisation | a bounded copy into the target array | `golden` (R14) |
| frame and static link | the emitter's own layout; a link is only emitted where the body reaches an enclosing local | `csmoke` (three-level nesting, R7) |
| `NEW` | `rt_new` allocates header plus payload and returns `NIL` on failure (report §6.4's failure case); it is the only route to the heap | `e2e`'s `a-failing-new-yields-nil`, which forces the failure with `OB_ALLOC_FAIL_AT` (the hook W7.4 added), and `fuzz` (which allocates through it) |
| descriptor read (`p.desc`) | the descriptor is at offset 0 of every view of a record, by the layout rule of §9.3 | `csmoke` (type-`CASE` and `IS`), `e2e`'s three-level extension |
| the trap report itself | writes to `stderr` and aborts; it reads no program data | `e2e`'s trap matrix: every row asserts the exact message *and* the source offset *and* that the process died by abort |
| a `SYSTEM` access (`GET`, `PUT`, `BIT`, `COPY`) | **not checked, by design**: the address is whatever the program computed, and this row is the escape the others are named against (§9.10) | `e2e`'s `system-*` rows (which run every entry, and whose round trip fails on a faulting address) and `csmoke`'s `system-*` cases (the snippets, executed on a local) |

Every check in the emitted code is locatable, not only the memory ones (W7.7):
`DIV by zero`, `MOD by zero`, `assertion failed` and `CASE: no matching type`
report `(at byte N of the source)` like the index and `NIL` checks do, because the
runtime has the offset in hand at each of those call sites. The two-argument
`rt_div`/`rt_mod`/`rt_guard` remain as wrappers over the positioned variants,
since `golden`'s hand-written fixtures call them without an offset.

The one row without a test is `rt_new`'s failure path, and the one access without
a check is the exception named in the pointer row. Both are itemised (W7.4, R26)
rather than implied.

Runtime checks are on by default (`-Ochecks`) and narrowed with `-Ochecks=<list>`
(W3.1, refined by W8.3): `none` or `n` removes every one, `all` is the default, and
the names are `index`, `nil`, `guard` and `case`, so a build can keep the checks a
given test is about and drop the rest — which is what the memory-safety rows want to
vary. An unknown name is a diagnostic, not a silent default. What the flag removes: the index bounds check, the `NIL` check on a
pointer access, a type guard's tag test, and the type-`CASE` fallback that traps
when no label matches — after which a violation is the program's problem, which is
the point of the switch. What it keeps, deliberately: the divisor checks, because
they are what keeps the *arithmetic* defined rather than merely unchecked (R4's
stance is that no emitted C relies on undefined behaviour), and `ASSERT`, which is
a language statement rather than a check. `-Ochecks=n` also removes the emitted
C's calls to `rt_index`/`rt_nil`/`rt_guard_is` entirely, which is what the suite
checks.
The trap payload is a message and the source position of the operation, so a
failing program says `foo.Mod:41: index 12 out of range 0..9`.

The complete set of helpers the emitter may reference — this is the whole runtime
interface. Every one of them has an executor in this repository
(`csmoke`, `golden`, or both), except `rt_new`, whose allocation the
fixtures stub out; `rt_trap` is exercised as a trap *counter* rather than a
process abort, which is what makes the trap rule testable at all.

| helper | signature | emitted for |
|---|---|---|
| `rt_div`, `rt_mod` | `int32_t (int32_t, int32_t)` | `DIV`, `MOD` |
| `rt_lsl`, `rt_asr`, `rt_ror` | `int32_t (int32_t, int32_t)` (`uint32_t` for `ror`) | the shifts |
| `rt_in` | `int32_t (uint32_t, int32_t)` | `x IN s` |
| `rt_set` | `uint32_t (int32_t, int32_t)` | `{a .. b}` |
| `rt_strcopy` | `void (char *dst, int32_t n, const char *src)` | string literal → `CHAR` array (pads) |
| | | *(`rt_charcopy` and `rt_charcmp` are executed in `golden`, the rest of the table in `csmoke`)* |
| `rt_charcopy` | `void (char *dst, int32_t n, const char *src, int32_t m)` | `CHAR` array → `CHAR` array (no padding) |
| `rt_charcmp` | `int32_t (const char *, int32_t, const char *, int32_t)` | `CHAR` array relations (bounded) |
| `rt_is` | `int32_t (const ObDesc *, const ObDesc *)` | `IS` |
| `rt_new` | `void *(int32_t, ObDesc *)` | `NEW` |
| `rt_trap` | `void (const char *)` | `ASSERT`, guards, bounds, `NIL`, a `CASE` default |
| `ob_register_commands`, `ob_run_command` | see §9.8 | module activation from the host |

### 9.5 Expressions **[every non-obvious rule verified in `csmoke`]**

| Oberon | emitted C |
|---|---|
| `x + y`, `x - y`, `x * y`, `x / y` | identical (same-typed operands are guaranteed by the checker) |
| `x DIV y` | `rt_div(x, y)` |
| `x MOD y` | `rt_mod(x, y)` |
| `x / y` on two integers | `rt_div(x, y)` — the folder defines integer `/` as `DIV`, and compile-time and run-time must agree |
| `c1 < c2` on two `CHAR` | `(unsigned char)` comparisons: a C `char` may be signed, and the report orders characters by ordinal |
| `p OR q`, `p & q`, `~p` | `(p \|\| q)`, `(p && q)`, `(!p)` |
| `s + t`, `s - t`, `s * t`, `s / t`, `-s` (sets) | `(s \| t)`, `(s & ~t)`, `(s & t)`, `(s ^ t)`, `(~s)` |
| `x IN s` | `(int32_t)((s >> (x & 31)) & 1u)` |
| `{a .. b}` | `rt_set(a, b)` = `((1u << (b-a+1)) - 1u) << a`, empty when `a > b`, width-32 case as `0xFFFFFFFFu << a` |
| `LSL(x,n)`, `ASR(x,n)`, `ROR(x,n)` | `rt_lsl`, `rt_asr`, `rt_ror`; `LSL`/`ASR` follow the report definition, so a count of 32 or more is 0 (or -1), and only `ROR` masks (R22) |
| `a < b` on `CHAR` arrays | `rt_charcmp(a, na, b, nb) < 0` — compares up to `max(na, nb)` bytes, treating the byte *after* an operand's length as NUL, so a filled array and a terminated one with the same content compare equal and the result agrees with `strcmp` wherever both are terminated **[verified]**: agreement with `strcmp` on terminated operands (all 25 ordered pairs), operands filled with no NUL at all, antisymmetry over 121 pairs, and ASan proving no read past either operand |
| `x = NIL` on pointers | `x == NULL` |
| `v IS T` | `rt_is(RT_DESC(v), &TD_T)` |
| `v(T)` (a type guard) | `(rt_guard_is(rt_is(RT_DESC(v), &TD_T), "type guard failed at byte N"), <narrowed view>)` — one *expression*, so the check keeps its place inside a short-circuit operand, and the offset makes a failure locatable. A pointer spelling tests `v != NULL && rt_is(...)` first, so a `NIL` pointer is rejected before its descriptor is read |

`rt_div`/`rt_mod` implement **floor** division **[verified by brute force over
all `x, y ∈ [-64, 64] \ {0}`]**:

```c
int32_t q = x / y, r = x % y;
if (r != 0 && (r < 0) != (y < 0)) { q -= 1; r += y; }
```

which satisfies the report's `x = q*y + r, 0 <= r < y` for `y > 0` and extends it
consistently to `y < 0`. Two hazards were found and are handled:

* `INT32_MIN DIV -1` traps in the hardware on x86; both helpers special-case
  `y == -1` **[verified]**.
* `ASR` via C's `>>` on a signed value is implementation-defined. `csmoke` uses
  `x >> n` (gcc, clang: arithmetic); the portable form
  `(x < 0) ? ~(~(uint32_t)x >> n) : (uint32_t)x >> n` was verified separately
  over 7 cases **[verified]** and is what the emitter should ship.

**`MIN(T)`/`MAX(T)`** (W8.2) are constants of the checker's making: it reads the
extreme from the type and records the value, and the emitter lowers the call to
that value, so nothing about them reaches the back end as a call. A constant
context folds them like a named constant (§8.5 rows 61-62). `MIN`/`MAX` are
*predeclared*, so a program that declares its own shadows them, and the recorded
value is keyed by the call's span, which is why nothing has to be checked for that.

**Evaluation order.** Oberon's reference compilers read left to right, and C
leaves the order of the operands of `+`, `*`, `/` — and of a call's *arguments* —
unspecified, while a call can write what another operand reads (R23). So the
operands of every operator *except* `&&`/`||`, and every *value* actual, are fixed
in a temporary in source order whenever any of them may have a side effect. Inside
a `&&`/`||` operand only the *declaration* is hoisted and the assignment stays a
comma expression, because that operand must not be evaluated when it is not
reached; `&&` and `||` themselves need no help, since C sequences them. A *record* value
actual's copy is part of this: inside such an operand only its declaration is
hoisted and the copy becomes a comma expression whose value is the copy's address
(R16). Two
consequences are worth stating: a `VAR` actual has to stay an address (it is not
materialised), and a comparison whose operands are the *same* call-free, trap-free
tree folds to a constant — structurally, and up to the commutativity of `+`, `*`,
`&` and `|` — because gcc proves the tautology and rejects the C otherwise (R20).
A `SYSTEM.ADR` operand is left in place too: it has no side effect, and its value
is wider than the `INTEGER` a temporary of its type would be (§9.10, R31).

### 9.6 Statements

| Oberon | emitted C |
|---|---|
| `d := e` | `d = e;`; for a `CHAR` array destination, `rt_strcopy(d, n, e)` from a string literal (length-checked, zero-padded) or `rt_charcopy(d, n, e, n)` from another array of the same length (no terminator implied) |
| procedure call | `f(&a, b, c);` — VAR actuals get `&`, structured value actuals get a caller-made copy |
| `IF … ELSIF … ELSE … END` | `if (…) { … } else if (…) { … } else { … }` |
| `WHILE g1 DO S1 ELSIF g2 DO S2 END` | `for (;;) { if (g1) {S1} else if (g2) {S2} else break; }` (report §9.6: repeat until no guard is true) |
| `REPEAT S UNTIL c` | `do { S } while (!(c));` |
| `FOR v := b TO e BY k DO S END` | `for (v = b; k > 0 ? v <= e : v >= e; v += k) { S }` — equal to the report's while-form for every `k != 0`, and `k == 0` is rejected by check 34 (`golden` checks all five range shapes against the desugared form) |
| `CASE c OF … END` (scalar) | `switch (c)` with one `case` per label and ranges flattened to consecutive labels |
| `CASE v OF T1: S1 \| … END` (type case) | `switch (RT_DESC(v)->id)` + arm-local cast |

The `FOR` header is exactly the report §9.8 equivalence, and its iteration counts
were checked against it for positive, negative, non-dividing and empty ranges
**[verified]**.

### 9.7 Procedures and calls

* **Naming:** `<Module>__<Name>` for module-level objects, and the lexical path
  for a nested procedure (`<Module>__<Outer>__<Inner>`). An *import alias* is
  resolved to the module it stands for, so `IMPORT O := Out; O.WriteLn` is
  `Out__WriteLn`. Locals, parameters and fields keep their Oberon name unless it
  is a C keyword (`int`, `while`, `restrict`, …), which is prefixed with `o_`.
  Nothing else needs escaping: an Oberon identifier cannot contain `__`, and the
  runtime's own names (`rt_`, `TD_`, `ob_`) never collide with `Module__Name`.
* **Parameters:**
  * value of basic type → C by value;
  * value of structured type (array/record) → the **caller** materialises a copy
    and passes a pointer, which gives the report's copy semantics and makes the
    read-only rule a compile-time matter rather than a C `const` accident
    **[verified]** for both a record (`value-param-*`) and an array
    (`array-value-param-*`): the callee sees the original and its writes are not
    visible to the caller;
  * `VAR x: T` → `T *x`, call site passes `&x`;
  * open array `ARRAY OF T` → `T *a, int32_t a__len`; `LEN(a)` is `a__len`
    **[verified]**;
  * procedure type → C function pointer;
  * nested procedures that reference enclosing locals get a hidden
    `void *<Module>__static_link` first parameter, and enclosing-frame access is
    `((Frame *)sl)->x`. This is sound because report §6.5 forbids a local
    procedure from being assigned to a procedure variable, so the only possible
    callers are lexical descendants. **[verified]** by the `nested-*` cases: a
    three-level nest mutates the outermost procedure's local by chaining frame
    pointers, a procedure that references nothing enclosing keeps the plain
    `void(*)(void)` signature (statically asserted with `_Generic`), and the
    address of a link-taking procedure is correspondingly *not* usable as a
    command pointer — which is exactly why the command table casts its entries.
* **Return:** the trailing `RETURN e` becomes `return e;`; a proper procedure
  ends with `return;`. Because `RETURN` cannot appear elsewhere (D6), there is
  never a second assignment site for a result.
* **Commands:** the module body can be activated from the host program, and
  report §11 defines what is callable that way — *"Individual (parameterless)
  procedures can thereafter be activated from the system, and these procedures
  serve as commands"*. So **the command table contains only parameterless
  procedures**, and the emitter simply does not register a parameterized one
  (registering `Write(ch: CHAR)` as a command would mean calling it through a
  `void (*)(void)` pointer, which is undefined behaviour):

  ```c
  /* only Out__WriteLn is parameterless, so only it is a command */
  static const ObCommand Out__commands[] = {
      { "WriteLn", Out__WriteLn },
      { NULL, NULL }
  };
  void Out__load(void) { Out__init(); ob_register_commands("Out", Out__commands); }
  ```  

  A host that wants to call an exported *parameterized* procedure links against
  `Out.h` and calls `Out__Write(ch)` directly, where the prototype gives it the
  right type. Verified end to end by the `command-*` cases in `golden`
  (dispatch by name for `WriteLn`, an unknown name and an unknown module both
  failing, and a parameterized procedure reached through `Out.h` rather than
  through the table).

### 9.8 Module initialisation and the exported interface

`Foo.c` contains the module body in `void Foo__init(void)`, which the emitter
records in `Foo.h` together with exported constants, types (as structs and
`typedef`s), exported variables (declared `extern`, assigned only from the
defining module), and exported procedure prototypes. Every module writes
`Foo.h` and `Foo.sym`; the `.h` is for the C compiler that links the program,
the `.sym` is for our own resolver (§7).

### 9.9 The driver **[implemented in `src/bin/obc.rs`]**

```
obc check Foo.mod [-I DIR]...                   diagnostics only
obc emit  Foo.mod [--out DIR] [-I DIR]...       .c / .h / .sym per module
obc build Foo.mod [-o EXE] [--out DIR] [-I DIR]... [--cc CC] [--cflags "…"]
obc run   Foo.mod [-I DIR]... [--command M.P] [--keep]
```

* **Module resolution.** Per `IMPORT M`, `M.mod` on the search path is compiled
  *depth first*, so a module's interface exists before the module that imports it
  is checked; otherwise `M.c` is taken as a hand-written implementation in the
  emitted shape, which is how the `Texts`/`Oberon` fixtures enter a build.
  `M.sym` must be findable either way — that is what the importer is checked
  against, and the reason §7 makes the format a first-class artefact.
* **The loader.** The driver generates `main`: it includes each module's header
  and calls `<M>__load()` in dependency order, then dispatches `--command M.P` if
  asked. Report §11's "the statements of a module are executed when the module is
  loaded" is exactly that call order, and dispatched commands are
  `ob_run_command` calls.
* **The C compiler.** Invoked with the flags §9.1 fixes, `-Werror` included, so a
  driver build is simultaneously the check that the emitted C is warning-free.
* **Build directories.** Temporary by default (so `run` leaves nothing behind);
  `--out DIR` and `--keep` keep the generated C for inspection.

### 9.10 Report §11.1's SYSTEM **[implemented in `src/cgen.rs`/`src/sema.rs`; `e2e`'s `system-*` rows run every entry, `csmoke`'s `system-*` cases execute the snippets]**

`IMPORT SYSTEM;` resolves to a **builtin** module (R5, `PLAN.md` W4.1): the
compiler supplies report §11.1's entries, so no `SYSTEM.sym` is read and no
`SYSTEM.mod` is compiled, and one on the search path is therefore *shadowed* —
a user supply would be a different language, while a module that names SYSTEM in
its import list must get the report's entries. The driver skips the import when
it walks the graph (`src/bin/obc.rs`) and the emitter emits no
`#include "SYSTEM.h"`; the emitted `.sym` *does* keep the import, so a client can
see that the module it depends on is not portable. `IMPORT S := SYSTEM` works
like any alias. **[verified]** by `e2e`'s `system-on-the-search-path-is-shadowed`
(a broken `SYSTEM.mod`/`SYSTEM.sym` pair on the search path changes nothing,
and no `SYSTEM.*` is written by that build) and
`check_tests::system_import_is_not_an_interface_lookup`.

The six entries of the report's table are lowered **in place**, with the `ob_sys_`
name space still the runtime's (there is no runtime helper here at all):

| Oberon | emitted C |
|---|---|
| `ADR(v)` | `(intptr_t)(uintptr_t)&v` |
| `SIZE(T)` | `(int32_t)sizeof(<C type of T>)` |
| `BIT(a, n)` | `((*(const uint32_t *)(uintptr_t)a >> ((uint32_t)n & 31u)) & 1u) != 0` |
| `GET(a, v)` | `v = <the word read through the address, in v's type>` |
| `PUT(a, x)` | `<a word store through the address, masking x to its own width>` |
| `COPY(src, dst, n)` | `memcpy((void *)(uintptr_t)dst, (const void *)(uintptr_t)src, (size_t)n * 4u)` |

**The address model, and why it deviates from the report's.** Report §11.1's
`mem[a]` is an array of *words* on a machine whose addresses are words; we define
an address as a **byte address** whose byte count is an `INTEGER`'s, so `ADR(v)`
is `&v` and `COPY`'s `n` counts words (`n * 4` bytes). `SIZE(T)` is the byte size
*including padding*, which is exactly the extent the record descriptors are built
beside — the same number `NEW` hands `rt_new`, so `SIZE(R) DIV 4` words are a
record's whole image, descriptor word included. `COPY`'s `n` and the address `a`
are `INTEGER`s; `SIZE` folds in C (it is a `sizeof`), it is not an Oberon
constant, so `SIZE` cannot appear in a `CONST` declaration (row 40).

The width of the *typed* operand is what a word access narrows to, and which
operand that is follows from the report's own signatures: `GET(a, v)` names the
variable it writes, so the load is narrowed to `v` (a `CHAR` keeps its low byte; a
`REAL` is one word, so the load *is* its bit pattern); `PUT(a, x)` names only the
value it stores, so the store is masked to `x`'s width (a `CHAR` contributes its
low byte and zeroes the rest of the word, which is what a word store means). A
mismatched pair (`PUT(ADR(r), i)`) stores the word the *value* occupies, which is
what an unchecked `mem[a] := x` says.

**`ADR` is as wide as the host's pointers, not as `INTEGER` (R31).** The
report's `ADR` returns an `INTEGER` and its addresses are 32 bits, which is the
whole model on the machine the report was written for; on a 64-bit host an
`int32_t` cannot carry a stack or `mmap`ed address, and the round trip the
lowering prescribes — an `int32_t` truncation, then `(uint32_t *)(uintptr_t)a` —
sign-extends that truncation and dereferences a *different* address. So the
expression keeps the host's width (`intptr_t`) and only narrows where the
program's own `INTEGER` narrows it: an assignment to an `INTEGER`, an `INTEGER`
parameter, or an arithmetic result the emitter fixes in a temporary. The one
expression whose C type is therefore not its Oberon type is an `ADR` call, and
the *only* thing the emitter does differently for it is to leave it out of the
operand sequencing above — it has no side effect, and a temporary of its Oberon
type would cut it in half, so the same expression would mean different things
depending on whether a sibling operand happened to need sequencing. **[verified]**
by `e2e/Sys.mod`, which takes the address of *locals* in a procedure (a stack
address) and of module statics and rounds a word trip through each, and by
`csmoke`'s `system-*` cases, which execute the snippets on a local. R31 records
the measurement that found it: with the plan's `(int32_t)` spelling that program
dies of SIGSEGV, and `(intptr_t)` round-trips `0x7ffff0686248` exactly.

**Everything else is deliberately unchecked.** No bounds check, no type check,
no alignment promise, and *no* dependence on `-Ochecks`: `SYSTEM` is the escape
hatch (§9.4), so a build with the checks compiled out emits byte-identical C for
it — **[verified]** by `e2e`'s `system-emits-the-same-c-under--Ochecks=n` (a
module whose only memory accesses are SYSTEM's) and by `csmoke`. What the checker
*does* enforce is what each entry takes (rows 55-59): `SIZE` a type name, `ADR` a
variable whose address exists, `GET`/`PUT` a basic type, `GET` a writable
variable, and the argument count of each. Entries the report lists as
implementation-specific — `VAL`, `ADC`, `SBC`, `UML`, `COND`, `LED` — are refused
*by name* (row 53: `SYSTEM.ADC is not implemented`), as is anything else the
report's table does not define; the *sized* accessors a byte-address model
invites (`GET8`…`PUT32`, `PLAN.md` W8.7) are **not** taken, because the report's
table has none and the address keeps the host's width instead, so a byte access
needs no separate entry.

---

## 10. Diagnostics

One `Diagnostic { span, severity, message, notes }` type, printed as

```
Bad.mod:4:3: error: cannot assign NIL to an INTEGER (report 9.1)
  |
4 |   x := NIL
  |   ^^^^^^^^
  = note: NIL may only be assigned to a pointer or a procedure variable (report 9.1)
```

and, for a syntax error, in the same shape:

```
Mix.mod:4:8: error: unexpected `;`
  |
4 |   x := ;
  |        ^
  = note: expected ident, integer, real, string, char or NIL (7 other token(s) would do as well)
```

* The span comes from the AST; `SourceMap::line_col(offset)` turns byte offsets
  into `line:column`, and the source line is echoed with a caret. No phase other
  than the printer needs line/column.
* Syntax errors go through the *same* path. `lalrpop_util::ParseError` is
  converted by `diagnostic_for_parse_error` into a `Diagnostic`, so a syntax error
  is rendered exactly like a semantic one — no phase prints its own format and no
  reader ever sees a byte offset **[verified by seven `e2e` checks and ten
  `diag_tests`]**. Three details that matter:
  * LALRPOP names terminals with its own quoting; the note strips it, so the
    reader sees `note: expected ; or END` rather than `Expected one of ";" or
    "END"`, and the list is trimmed to six names plus a count of the rest.
  * An unexpected-token or unexpected-end-of-file diagnostic gets the *expected*
    set as a note, and a recovered error additionally gets `the parser skipped N
    token(s) to carry on`, which is what tells the reader that a later message
    may be a consequence rather than a second mistake.
  * A lexical error (`LexicalError`) carries its offset and a note stating the
    vocabulary rule (`not part of Oberon's vocabulary (report 3)`), because its
    `Display` would otherwise be a bare byte offset.
* Notes **accumulate** (`Diagnostic::notes`): a recovered error carries two, and
  both matter — what would have fitted here, and what the recovery discarded.
  Rendering only the last one used to hide the expected-token note, which is how
  a missing `;` came to be answered with an operator list and no mention of `;`.
* The expected-token note **leads with `;`** when `;` would have fitted, because a
  missing separator is the commonest defect, and the recovery note **quotes the
  tokens it skipped** (``the parser skipped `y := 2` to carry on``) rather than
  only counting them: the statement the recovery discards is the innocent one,
  and its own errors are never reported, so the reader has to be told which text
  it was.
* **A name is reported once.** Two uses of one undeclared name are one mistake:
  the first use carries the message and the count of the rest
  (``= note: 2 further uses of `Zz` are suppressed``), which is what a C compiler
  does too. Two *different* names remain two diagnostics.
* **An error limit**, `--ferror-limit N` (default 20, `0` = no limit): a file with
  sixty broken statements prints twenty diagnostics and one line saying how many
  were suppressed. Warnings are never dropped, the exit code is unchanged, and
  below the limit nothing changes at all — which the suites check by asserting
  exact counts on the small cases.
* The **poison rule** is deliberate rather than incidental: a type or constant
  that failed to resolve produces an `ErrorType`/error symbol, so its later uses
  say nothing rather than reporting the same failure again — pinned by
  `diag_tests::a_failed_type_or_constant_poisons_its_uses`.
* Diagnostics are de-duplicated by *(span, severity, message)* and sorted by
  span, because the recovering parser and the checker can meet the same mistake:
  a call statement used to be looked up twice, so an unknown callee was reported
  twice **[found by `diag_tests::one_mistake_is_reported_once`, fixed, and
  verified]**.
* Every check in §8.5 emits at most one diagnostic per site, and `ErrorType`
  keeps a single error from cascading.

---

## 11. Testing strategy

The same commands run on a clean checkout in CI (`.github/workflows/gate.yml`),
in the order PLAN.md and README.md list them, so "the gate" is a machine's habit
rather than a human's.

| level | what it covers | how |
|---|---|---|
| lexer | the near-miss table in §4 | unit tests on token streams **[exists]** |
| parser | report examples, all statement/expression forms, guard/call unification, unqualified-vs-qualified names | parse tests asserting AST shape **[exists: 7 tests]** |
| parser (negative) | syntax errors, illegal characters, unterminated strings/comments, and the *report's own* restriction that `RETURN` is not a statement | `unwrap_err` assertions **[exists]** |
| diagnostic rendering | byte span → `line:col`, the source line, the caret, notes, warning vs. error, one message per mistake | `src/lib.rs::diag_tests` **[exists: 18 tests]** |
| error recovery | statement-level resynchronisation: several syntax errors in one run, syntax *and* semantic errors in one run; a fatal module-level error and a lexical error each reported once, with a location | `diag_tests` + `e2e` checks 10–13 **[exists]** |
| resolver — reclassification (D2/D3) | guard vs. call, module-qualification fold, unresolved-selector invariant, a bad record `CASE` label | unit tests in `src/lib.rs` **[exists: 3 tests]** |
| resolver + checker | one case per row of §8.5 (rows with two shapes have two cases), plus the report's `Out` module and the kitchen sink checking clean | `src/lib.rs::check_tests` **[exists: 5 tests]** |
| diagnostic rendering | byte span → `line:col`, message text | `check_tests::diagnostics_carry_usable_spans` **[exists]** |
| constant folder | `DIV`/`MOD`/shifts/sets fold identically to the runtime helpers | `golden/constexpr.tsv` cross-check **[exists: 52 expressions]** |
| C emission | emitted C compiles with `-Wall -Wextra -Werror` and the program's stdout matches the expected output | `e2e` **[exists]** |
| type algebra | identity, extension, assignment, parameter modes, operators, predeclared signatures | `src/types.rs` **[exists: 12 tests]** |
| constant folder | all four folding contexts, plus agreement with the emitted C (`constexpr.tsv`, 52 expressions, produced by the C side from its own helpers) | `src/const_eval.rs` **[exists: 7 tests]** |
| runtime helpers | §9.5's tricky mappings, the type-`CASE` switch, static links, parameter modes | `csmoke` **[exists: 67 cases]** |\n| the *emitted* runtime | the same vectors, compiled against `src/oberon_h.inc` itself rather than a hand-copy | `golden/rt.h` includes it **[exists]** |
| whole-program shape | statement lowering of the report's procedures, module init, command table, exported interface, read-only exports | `golden` **[exists: 66 cases, plain + ASan/UBSan]** |
| regressions found by review | one output line per defect found in the code review: integer `/`, CHAR ordinal order, folded constants, `CASE` ranges and multi-labels, record-copy descriptors, a local record as a `VAR` record parameter, open-array forwarding, a call through a record field, `PACK`/`UNPK`, `NEW` on a recursive type | `e2e/Regress.mod` + `regress.expected.txt` **[exists: 11 lines]** |
| compile-only regressions | a recursive record type; Oberon identifiers that are C keywords; a *failing* guard Dtraps with its offset | `e2e/run.sh` checks 8 and 9 **[exists]** |
| the compiler itself | emit C for five modules (the report's `Out`, a hello-world, a language torture program, and a deliberately trapping one), compile that C with `-Werror`, run it, compare with an independent expectation, check that a trap names the source offset, and do it once more through the driver (`obc build`/`run`/`check`) | `bash e2e/run.sh` **[exists: 26 checks]** |
| interface format | round trip, fixed point, parameterless procedures, rejection of an illegal parameter type | unit tests in `src/sym.rs` **[exists: 3 tests]** |
| random programs | the generator's fragment: `INTEGER` arithmetic, the four shift builtins, `IF`/`WHILE`/`REPEAT`/`FOR`/`CASE`, procedures and recursion, each program carrying its own expected values | `bash fuzz/run.sh` **[exists: `src/bin/obgen.rs`; in the gate — `fuzz/regressions.txt` names every seed that once found a defect, then a coverage block runs]** |
| the seed corpus as a regression suite | each seed that once found a defect, kept in a fixed list with the reason it is there, then a block for coverage; a seed reproduces a program exactly on any machine | `fuzz/regressions.txt` + `bash fuzz/run.sh` **[exists: 9 listed seeds + 100 coverage seeds]** |
| the *emitted* code under sanitizers | every generated program built with `-fsanitize=address,undefined -fno-sanitize-recover=all`, leak detection on, so a clean run is evidence about the code rather than about the program's arithmetic | `SAN=1 bash fuzz/run.sh` **[exists: 300 seeds clean, plus the regression list in every default run]** |
| the trap paths, deliberately | each generated program ends with one violation that *must* trap (index past the end, NIL dereference, a guard that cannot hold, a computed zero divisor, a false assertion), and the run must produce the exact trap line — message *and* source byte — which the generator computes from the same render | `bash fuzz/run.sh` (`--adversarial`) **[exists: 200 seeds + the regression list]** |
| whole language | the report's example modules (`Out`'s `Write`/`WriteInt`/`WriteLn`, `log2`, `ReadInt`) compiled and executed against a `Texts`/`Oberon` stub | `e2e` + `golden` **[exists]** |

The two existing suites are kept as-is rather than rewritten: this crate keeps the
grammar honest during development, and `csmoke` is the specification of the
runtime helpers in executable form.

---

## 12. Milestones

| # | deliverable | acceptance |
|---|---|---|
| M0 | lexer + grammar + syntax AST | parses the report's example modules and a module exercising every construct; no LALRPOP conflicts; **done, 10 tests green** |
| M1 | resolver: scopes, imports, interface files, qualification fold, guard/args split, `end_name` | every well-formed module in the test corpus resolves; every §8.5 row in classes 1–5, 48 reported — **done**, `src/sema.rs`, with the report's `Out` module checking clean |
| M2 | checker + constant folder | all §8.5 rows implemented, each with a matching negative test — **done** except the four explicitly listed gaps in §8.5 (field visibility, one half of row 50, row 54's procedure half) |
| M3 | C emitter for types, declarations, expressions, and the four control constructs; runtime header | `Out.Mod` compiles to C, compiles with `cc -Wall -Wextra -Werror`, and prints the same as a hand-written reference — **done**, `src/cgen.rs` + `e2e` |
| M4 | procedures, nested procedures, parameter modes, open arrays, `NEW`, `IS`/guards, type `CASE` | a corpus module with dynamic dispatch by `IS` runs correctly — **done**: `e2e/Torture.mod` exercises all of it, including a three-level nested procedure reaching an enclosing local |
| M5 | modules, imports, commands, exported interface files | two-module program compiles and runs; the report's `Out`/client split works — **done**: the e2e suite compiles `Texts`, `Oberon`, the report's `Out` and `Torture`, and the compiler writes and reads the `.sym` files between them |
| M6 | diagnostics pass, `-Ochecks`, golden tests | compiler is usable on a few thousand lines of Oberon-07 |
| M7 | driver: import-graph resolution, loader generation, C invocation, run | `obc run Hello.mod` produces a runnable program; `obc run Torture.mod --command Torture.Run` matches the expected output — **done**, `src/bin/obc.rs` |

The open work is itemised in `PLAN.md`, in the order it is worth doing, with the
section or risk each item comes from.

---

## 13. Risks and open questions

| # | risk | state / mitigation |
|---|---|---|
| R1 | The report's grammar is not LR(1) in three places. | **Found and resolved**, not papered over: D2 and D3 move two decisions to the resolver; D1 is a local factoring. Evidence in §5. |
| R2 | Type guard vs. call argument list is decided by a *semantic* rule. | Sound (types and variables share one namespace), pinned by tests. The residual case `f(x)(y)` is diagnosed rather than mis-parsed. |
| R3 | No garbage collector. | **Implemented** (W10.2, design in §15): mark-sweep and non-moving, a precise mark driven by the descriptor's `ObFld` map, a conservative stack and register walk, one header per block in front of the payload, collection before `NEW` gives up (either the `malloc` failure or the `OB_GC_LIMIT` byte budget) and once at exit. Roots are each module's own variables, registered from `__init`. Evidence: `e2e`'s six collector rows (a 20000-allocation program in a 4096-byte heap in the stress, limit and idle modes; a live set over the limit reaching `NIL`; one root of each kind, including an `ARRAY OF POINTER` and a map from another module's translation unit; and a pointer field the *base* of the object's type declares) and `fuzz`'s stress blocks, which build the corpus with the sanitizers because a freed object whose memory is not reused yet still reads correctly. `NEW` failures still yield `NIL`, which is specified. |
| R4 | Signed overflow: C's UB vs. Oberon's unspecified wrap. | `-fwrapv` in the documented compile line **[verified]**; the emitted code never relies on the C compiler's optimiser reasoning about overflow. |
| R5 | `SYSTEM` (§11.1) was not implemented. | **Implemented** (`PLAN.md` W4.1): the import resolves to a builtin module (§9.10), the report's six portable entries are lowered in place, and the RISC-specific extras (`VAL`, `ADC`, `SBC`, `UML`, `COND`, `LED`) are still refused *by name*. R31 records the one thing the report's model does not give on a 64-bit host. |
| R6 | `FOR` loops that end at the largest `INTEGER`: `v += k` wraps and the guard is re-tested against a wrapped value. | **Resolved**: the report's own equivalence (§9.8) is `v := beg; WHILE v <= end DO S; v := v + inc END`, so the wrap and the re-test *are* the specified behaviour, and with `-fwrapv` it is defined C. No check is added, and W3.1's flag does not need one: a `FOR` loop cannot exceed its bound. |
| R7 | Nested procedures need a static link. | Only emitted when the nested body actually references an enclosing local; procedure *variables* cannot hold local procedures (report §6.5), so the link is always valid where it is used. **Verified** in `csmoke` (three-level nest, chained frame pointers, plain-signature case). |
| R8 | `CASE` exhaustiveness on records is only a warning. | The report does not require it; Wirth's compilers report it. We warn, and the emitter always emits a trap for the no-match case **[verified]**. |
| R9 | `LEN(v)` is defined for one-dimensional open arrays only. | Report §10.2 defines exactly that. Multi-dimensional open arrays are Oberon-2. |
| R10 | Char classification of `BYTE` ↔ `INTEGER`. | Implemented as a two-way compatibility in `is_assignable`, per report §6.1; it is the only implicit conversion in the language. |
| R11 | The `csmoke` spec itself contained an error: it asserted `ROR(0x80000000u,1) == 1`, which is `ROL`. | **Found and corrected** by the smoke run; the design uses true right-rotation. This is why the spec was executed rather than trusted. |
| R12 | Registering a *parameterized* exported procedure in the command table would invoke it through a `void (*)(void)` pointer — undefined behaviour. | **Found** by the `golden` link model and fixed in §9.8: only parameterless procedures are registered, which is what report §11 defines a command to be. |
| R13 | With `BY = 0` the emitted `FOR` header and the report's while-desugaring disagree (infinite loop vs. zero iterations). | Unreachable: check 34 rejects `BY = 0`. The equivalence in §9.6 is therefore stated for `k != 0` only, which is every program the checker accepts; `golden` compares the two forms on all five range shapes. |
| R14 | `rt_strcopy` leaves no NUL when the source exactly fills the destination, and a later `strcmp`-style comparison would read past the array. Reachable via a same-length `ARRAY n OF CHAR` array assignment, which §8.2 permits. | **Found** by `golden` and fixed in two places: §9.6 distinguishes the padded string-literal copy from the bounded array-to-array copy, and §9.5 compares `CHAR` arrays with `rt_charcmp`, which is bounded by the shorter operand and stops at a NUL in either — it never assumes termination. |
| R15 | A **failing** guard written with the *record* spelling reads past the object by construction (`p(S).b` where `p` points at an `R`), and the C compiler could see that statically: `-Warray-bounds`/`-Wmaybe-uninitialized` then rejected the program under `-Werror`, even though the guard's test traps first. | **Fixed** (PLAN.md W2.2), and the cause was not the guard at all: `rt_new` was `static inline` in the runtime header, so gcc folded `malloc(sizeof(R))` into the caller and could prove from *that* that the guarded write lay outside the allocation. `rt_new` is defined in `oberon_runtime.c` now, which no translation unit can see through without LTO; the guard's own check still traps first, with its offset. Both spellings are now tested: `e2e`'s `trap-guard-record-spelling-as-target` (gcc) and `guard-record-spelling-clang-traps` (clang, the same program), beside the pointer spelling's `failed-guard-traps`. Accepted residual: a *failing* guard that reads a derived field under `-fsanitize=address` is still a report rather than a trap, since ASan sees the access at the same point the guard checks it. |
| R16 | A structured (record) value argument inside a short-circuit operand was copied one step earlier than Oberon evaluates it, and a call in the actual's own selector therefore ran even when the operand was not reached. | **Fixed** (PLAN.md W2.3): inside a `&&`/`||` operand the copy's *declaration* is hoisted (which evaluates nothing) and the copy itself becomes a comma expression whose value is the copy's address, so the copy — and any call inside the actual's selector — happens exactly where the language evaluates the operand. Pinned by `e2e`'s `copy-inside-shortcircuit`, which counts the calls in both directions: none when `FALSE & …` short-circuits, one when `TRUE & …` does not. The row is sensitive: with the old code the count is 1 in the first case and the program's own check traps. |
| R17 | `resolve.rs` classified guards and module qualification with *module-wide* name sets, so an inner declaration did not shadow them: a procedure-local `Texts` was still the import alias and a procedure-local `T` was still the type. | **Fixed.** The pass carries a scope chain now (`resolve.rs`: one scope per procedure, with its parameters and declarations; every declaration is recorded, not just types and import aliases, because a variable of some name shadows a type of that name). Evidence: before the fix, `Texts.f` with a local `Texts` was rejected as "`Texts` is not a module" and `Q(T)` with a local `T` as "`T` is not a type"; both check clean now, pinned by `check_tests::an_inner_declaration_shadows_a_type_name_and_an_import_alias`. |
| R18 | `--cflags "…"` is split on whitespace, so a single flag whose own argument contains a space cannot be written that way. | Mitigated by `--cflag ARG`, which appends one argument verbatim and is repeatable; both are in the driver's usage. A path with spaces is passed correctly as a *file* argument, because the driver never goes through a shell. |
| R19 | Declaration-level and module-level syntax errors get no recovery: every shape that would express it is rejected by LALRPOP with a local ambiguity on the error token, because the report lets each declaration section be empty and the section's ε production then competes with the error token. | **Measured**, not assumed: four shapes were tried, all four rejected (D7). The outcome is one located diagnostic instead of a cascade, and statement-level recovery covers the common case. |
| R20 | The emitted C is not warning-clean for *legal* programs, so a program the checker accepts can fail to build under the documented line. Six causes, all found by the generator: a constant subexpression that overflows; a parameter the body never reads; a comparison of two constants; a comparison of a tree with itself; an unused `static`; and a gcc-folded *symbolic* overflow. | **Found by the generator** (W6.2 and its standard suite) and **fixed**: the folder folds trap-free self-comparisons structurally and commutative reorderings, the emitter sequences operands and arguments, and unused parameters and module statics are marked with `(void)x;`. The one cause a fold cannot reach — gcc proving an overflow after cancelling a variable — is answered by `-Wno-overflow` in the documented line, whose justification is the same as `-fwrapv`'s (§9.1). Values remain checked by the suites, so silencing the warning cannot hide a wrong answer. |
| R24 | `ABS(x)` used to be emitted as `((x) < 0 ? -(x) : (x))`, so an operand containing a call was evaluated twice, possibly with two different results. | **Found by review** of the fold work and fixed: `rt_abs` takes its operand once. The same duplication class is worth watching wherever an operand is emitted more than once; the fold now refuses any tree containing a call, which is also what keeps a *shadowed* builtin (`ABS` declared by the program) from being folded away with its side effects. |
| R25 | The checker declared the predefined identifiers and the module's own declarations in *one* scope, so a module-level `ABS` was rejected as a duplicate instead of hiding the predefined one, although report §4 makes the universe the outermost scope. | **Found while fixing R24's review finding** — the reviewer's shadowed-builtin example could not even be written. Fixed by opening the universe as its own scope; `emit_tests::a_shadowed_builtin_call_is_not_folded_away` pins both halves. |
| R23 | The emitted C relied on the C compiler evaluating operands in source order, which C does not guarantee, while Oberon's own reference compilers read left to right: an expression where one operand is a call that writes a variable another operand reads can therefore differ. It applies to an operator's operands *and* to a call's arguments, both of which C leaves unordered. | **Demonstrated and fixed.** The evidence, in order: (1) instrumenting the failing seed showed exactly one of its twelve checked variables differing (`m0` = 5 against the model's 3); (2) a *by-hand* rewrite of the two suspicious statements into explicit temporaries — `t := F0(F0(MIN)); t := t + LSL(F0(MAX), 33); m0 := t` and `t := m0; t := t + R(3); m0 := t` — made the module's own checks pass, which is what proved the order rather than the arithmetic; (3) the generator's shift model and the runtime agree with each other and the report (`LSL(1,33)=0`, `ASR(-1000,33)=-1`, `LSL(1,31)=MIN(INTEGER)`, `LSL(3,32)=0`), so R22 was intact on both sides. Fixed by §9.5's evaluation-order rule: operands of every operator but `&&`/`||` and every value actual are fixed in temporaries in source order when any of them may have a side effect. A later seed (`F0(ABS(F0(100, acc)), LSL(ASR(m0, 0), 1))`) showed the same hole for *arguments*, and a third showed it *inside* a `&&` operand, where the assignment stays a comma expression so that a short-circuited operand is still not evaluated. The seeds that found all of this are in `fuzz/regressions.txt` (7, 9, 17, 1098, 1116). |
| R22 | `LSL` and `ASR` masked their shift count with `& 31`, which is what `ROR` does but not what the report defines: §8.2.2's table gives `LSL(x,n) = x * 2^n` and `ASR(x,n) = x DIV 2^n`, so a count of 32 is *defined* (0, or 0/-1) rather than a no-op. The constant folder and the runtime helpers agreed with each other and both disagreed with the report, and four places documented the masking as intended. | **Found by the generator** (PLAN.md W6.5): a generated program's self-check trapped, and the mirror — which follows the report — was right. Fixed in the runtime header, the folder, `csmoke` (whose spec had encoded the masking, as R11's did before it), `golden`'s case list and the second-pass table, plus the four document mentions. |
| R27 | A `VAR` open-array parameter was emitted as `(*v)[i]` — the parameter is *already* the element pointer, in both modes (§9.2), so that does not compile — and once that was fixed, an open-array index had **no bounds check at all**, leaving `v[4]` against a two-element actual to corrupt memory. | **Found by the trap matrix** (PLAN.md W7.4), which is what it is for: the C compiler rejected the first, and the second showed as a program that ran to completion instead of trapping. Fixed in `cgen::access` (no deref for an open array) and in the index arm (`rt_index` against `name__len`, through `access_len` for a parameter of an enclosing procedure). |
| R29 | A designator narrowed by a **type-`CASE` arm** carries the narrowed C type in its *spelling* (`((S *)p)`) rather than in its *type* alone, so the name stops being an lvalue: `p := NIL` inside `CASE p OF PS: … END`, `INC(p)`, `NEW(p)` and `PACK(p)` there do not compile. (A *field* access through a narrowed name kept working, because a cast is a fine base for a member store; what failed was the narrowed name *itself* as the target.) | **Found by the trap matrix** (PLAN.md W7.4) while writing the type-`CASE` row, whose arm had to be left empty; fixed in W7.6. The fix is smaller than the row first proposed: the plain spelling is used where a store is needed, and only where the narrowed name is the *whole* designator — `field_path` walks *down* the base chain and is called with the *derived* record, so with a selector after the name the cast has to stay for a field's offset to be right. The guard's check is hoisted to a statement in the same position (which the assignment emitter and the builtins that take a `VAR` argument flush), which also removes R15's unnecessary cast there. Rows: `e2e`'s `narrow-assign-to-narrowed`, `narrow-field-through-narrowed`, `narrow-guard-as-target`. |
| R30 | `rt_new` took its size as `int32_t`, so routing `NEW` through it (the fix for the matrix's `NEW` row) truncated a record larger than 2 GiB to its low word: the allocation was `sizeof(T)` no longer, and the default `-Werror` build rejected the conversion. | **Found by review** of the W7.4 commit pair, by source analysis rather than a run, with the input that shows it (`ARRAY 1073741824 OF INTEGER`, 4 GiB). Fixed: the parameter is `size_t`, as `malloc`'s is, so the size `NEW` computes survives the call; the compile of that record is the evidence. |
| R28 | An assignment **through a guard** (`p(S).f := 1`) emitted the guard's comma expression in an lvalue position, so the program did not compile at all. | **Found by the trap matrix** while adding the guard row (W7.4). A comma expression is not an lvalue; the guard's check is now hoisted into the prelude — which the assignment emitter already flushes after rendering its target — and the view alone is the expression. The *static* problem the design already records as R15 (gcc sees a read out of the allocation's bounds when the guard *fails*) is untouched and still documented. |
| R26 | `rt_nil` reads its operand twice, so an operand with a side effect — an index expression containing a call, as in `a[Next()].x` — would be evaluated twice and could check one pointer while dereferencing another. | **Found by review of R21's fix.** The check is therefore emitted only while the designator built so far is side-effect-free (`cgen::has_call`), which is sound: the case keeps the old fault rather than gaining a wrong check. Completing it needs the operand hoisted into a statement, which is the emitter's temporary machinery (R16) and is itemised in `PLAN.md` W7.1. |
| R32 | The emitter could not name a type declared later in the module: `TYPE P = POINTER TO R; R = RECORD … END; VAR a: ARRAY 2 OF P` emitted the array before the record and called the record `Anon_1` where every other use said `T3__R`, so a legal program failed to compile (`unknown type name`). The cross-module sibling named a pointee `Def__Anon_1` in the header and `Def__Node` in the interface, so the two disagreed. | **Found while building the collector** (PLAN.md W10.2) — the compiler at HEAD emitted the same thing, so it was neither new nor caused by it — and **fixed in W2.4**, by the two changes its causes called for: the module's declared type names are collected *before* any type is visited, so a composite reached through a pointer (which is what an array of pointers does) keeps its own name rather than getting an anonymous one; and every named composite gets a forward `typedef struct X X;` before all the definitions, so the order `type_rank` chooses no longer matters for a pointer use. The definition repeats the same typedef, which C11 allows and `-Wall -Wextra` accepts. Rows: `e2e`'s `pointer-array-of-a-later-record`, `pointer-to-a-record-in-another-module` and `neither-side-names-the-pointee-anonymously`. |
| R31 | The report's address is a 32-bit `INTEGER` (§11.1's `mem[a]`), and the lowering `PLAN.md` W4.1 first prescribed — `ADR(v)` → `(int32_t)(uintptr_t)&v`, then a word access through `(uint32_t *)(uintptr_t)a` — truncates it and sign-extends the truncation on the way back. | **Found while writing W4.1's own test**, and measured rather than argued: a stack address does not survive that round trip — `&v = 0x7ffff068624c` truncates to the `int32_t` `-261594548` (`0xf068624c`) and casts back to `0xfffffffff068624c`, so the store faults; the module built and the program died of SIGSEGV, while the `intptr_t` form returns `0x7ffff0686248` unchanged. **Fixed** by keeping the address as wide as the host's pointers in the *expression* (`ADR(v)` → `(intptr_t)(uintptr_t)&v`), narrowing it only where the program's own `INTEGER` does; the only other emitter change this needs is that an `ADR` operand is never fixed in a sequencing temporary, whose type would be that `INTEGER` (§9.10). `e2e/Sys.mod` takes the address of *locals* and `csmoke` executes the snippets on a local, so the defect cannot come back silently. What remains, documented rather than fixed: storing an address in an `INTEGER` — the report's own spelling — is lossless only while the host's addresses fit in 32 bits, which on this host is static data and not the stack. |

Decisions taken during implementation, recorded because the report is silent
or self-inconsistent and a compiler has to choose once and then be consistent:

1. **How a guard or a type-`CASE` label names its type.** The report states the
   rule as "T is an extension of the declared type T0 of v" (report §8.1,
   §8.2.4) but spells T two ways for a *pointer* `v`: §8.1's own example is
   `t(CenterNode)` — a **record** name, CenterNode extending the pointer's base
   type — while §9.5's is `CASE p OF P0` with **pointer** names. Both spellings
   are accepted, and the guard's type is the type named, which is what makes
   §8.1's `t(CenterNode).subnode` come out as `Tree`. Implemented in
   `Sema::guard_type`;
   the alternative (pointer names only, Oberon-2 style) would reject the
   report's own example.
2. **The `FOR` control variable's scope.** The report only says it is an integer
   variable; the first draft of this design added "of the current scope", which
   the report does not say and which rejects perfectly ordinary programs that
   count with a module-level variable. Any visible `INTEGER` variable is
   accepted, provided it is not read-only (which covers imported variables) and
   is not assigned inside the body.
3. **A field's `*` is legal** (report §6.3: public fields are marked,
   unmarked fields are private). An earlier draft rejected it.

4. **`x / y` on two integers is `DIV`.** The report says `/` applies to numeric
   types with the result having the operands' type, and defines `DIV`/`MOD`
   precisely, but never says how `/` rounds; the reference compilers use the same
   code for both, so we do too. (The alternative — rejecting `/` on integers and
   forcing `DIV` — is gratuitously incompatible.)
5. **`CHR(x)` truncates** (`(x & 0xFF)`) rather than rejecting an ordinal above
   255, so the folder cannot disagree with the emitted `(CHAR)x`.
6. **`ORD` of a `SET` is not folded.** Report §10.2 lists `SET` among `ORD`'s
   argument types, but a set has no single ordinal number, so no constant exists
   to produce; the folder reports that rather than inventing one.
7. **Oberon-2 is not supported, and the reason is recorded rather than
   assumed.** Everything in `PLAN.md` W8 turned out to be a piece of Oberon-2, so
   the question "are we becoming an Oberon-2 compiler?" was answered explicitly:
   the additive pieces (`CASE … ELSE`, `EXIT`, early `RETURN`, `MIN`/`MAX`) are
   taken on their own merits, while numeric *inclusion* (which would replace this
   document's explicit no-implicit-conversion rule), type-bound procedures with
   their dispatch tables, and heap/nested open arrays are declined until a
   *named* program needs them. `PLAN.md` W8.11 holds the delta table, the
   mechanism (a gated grammar, §5), the scope boundary — the language only, since
   ETH's library and system modules are a separate port — and the trigger that
   would change the answer.
8. **A field may hide an inherited field's name, with a warning.** Report §6.3
   permits it (a derived record may declare a field whose name the base already
   has), and rejecting it would reject legal programs; but `x.f` then means the
   derived field silently, so the checker warns. Implemented in the record
   declaration path, per field name.
9. **`ARRAY 0 OF T` is an error** — an implementation restriction, not a report
   rule: a zero-length array has no legal index, so every access to it would trap
   (report §6.2 is silent; ETH's compilers reject it). Pinned by catalogue row 41.
10. **Unused imports are warnings, on by default.** A warning that is off by
   default is a warning nobody sees; the CLI prints it, it does not change the
   exit code, and the suites' clean modules import nothing unused.
11. **A single-character string is usable where a `CHAR` is expected** (`ORD("0")`,
   and assignment per §9.1.2), but is not an integer operand.
13. **Four additive extensions, on by default** (`PLAN.md` W8.1, W8.2, W8.4, W8.5).
    `CASE … ELSE`, `MIN(T)`/`MAX(T)`, `EXIT` and `RETURN` as a statement are not
    report Oberon-07, and they are adopted rather than gated — `ELSE`, `RETURN` and
    `EXIT` are keywords the report already reserves or that no report program may
    use as an identifier once reserved, and `MIN`/`MAX` are predeclared names a
    program can shadow. The rule that decides which extensions may be: an extension that
    *only adds* — a report program cannot be affected by it — may be on by default,
    while one that changes how existing text tokenises or resolves needs a dialect
    switch (W8.10 is the case in point, since the report makes capital and
    lower-case letters distinct). `ELSE` is already a keyword and `MIN`/`MAX` are
    predeclared *names* that a program can shadow, so neither can change what an
    existing program means. W8.11's survey records the rest: the ones with a real
    cost (numeric inclusion, type-bound procedures, `WITH`) stay rejected, and the
    syntaxes that are another spelling of machinery we already have are not worth
    their keyword.

    **The one extension that needed the switch is implemented behind it**:
    `-Odialect=lower` folds reserved-word case (§3). It is the case the rule above
    exists for — a report program may write `Begin` as a variable, and folding would
    tokenise that as the reserved word — so it is off unless asked for, its
    collisions explain themselves in the diagnostic, and the dialect is recorded in
    the interface header. `MIN`/`MAX`'s aliasing of the predeclared *names*
    (`integer`, `nil`) is the additive half W8.10 describes and is deliberately not
    part of it.

12. **The SYSTEM address model** (report §11.1, `PLAN.md` W4.1). The report's
   `mem[a]` is an array of *words* on a machine whose addresses are words, and it
   types `ADR` as `INTEGER`; this implementation defines an address as a **byte**
   address, so `ADR(v)` is `&v` and `COPY`'s `n` counts words (`n * 4` bytes), and
   it keeps that address as wide as the *host's* pointers in the expression rather
   than in an `INTEGER` — because on a 64-bit host the report's own 32-bit
   spelling truncates a stack address and dereferences the truncation (R31,
   §9.10). `SIZE(T)` is the byte size including padding, consistent with the
   descriptors, and the sized accessors W8.7 suggests are not taken. A module that
   imports SYSTEM is not portable, and its interface file records the import so a
   client can see it.

---

## 14. Evidence appendix

Commands are run in this directory.

```
$ cargo test
test diag_tests::a_lexical_error_names_the_character_and_what_to_do ... ok
test diag_tests::a_missing_end_is_reported_where_the_module_gives_out ... ok
test diag_tests::a_syntax_error_is_located_explained_and_underlined ... ok
test diag_tests::a_warning_is_rendered_as_a_warning ... ok
test diag_tests::one_mistake_is_reported_once ... ok
test diag_tests::recovery_reports_later_errors_in_the_same_run ... ok
test diag_tests::semantic_errors_accumulate_with_their_own_locations ... ok
test diag_tests::several_syntax_errors_are_reported_in_one_run ... ok
test diag_tests::the_common_mistakes_explain_themselves ... ok
test diag_tests::the_source_line_is_echoed_even_when_the_span_is_at_the_end ... ok
test tests::type_guard_and_call_share_one_production ... ok
test sym::tests::round_trips_every_item_kind ... ok
test result: ok. 47 passed; 0 failed
```

* The semantic pass is checked on **valid** programs first: the report's own
  `Out` module (whose imports resolve through interface files) and a module
  using every construct of the report both produce zero diagnostics, and the
  second produces exactly the two unused-import warnings it should. Bugs found
  this way, before any negative case was written: a forward `POINTER TO Node`
  captured an empty record placeholder instead of the defined record; a
  non-record type referenced before its declaration was reported as "not
  declared" instead of naming the forward-reference rule; `ORD("0")` and
  `ch <= "9"` (the report's own code) were rejected because a one-character
  string was not treated as a `CHAR`; read-only-ness was lost when selecting a
  field of a read-only parameter, so `v.x := 1` on a structured value parameter
  slipped through; a record field's `*` was rejected although report §6.3
  defines it; and the `FOR` control variable was required to be in the current
  scope, which the report does not say (all six fixed, all six now covered by a
  case).
* `check_tests::catalogue_rows_report_what_they_should` carries one case per row
  of §8.5, each naming its row number, plus an absence case (a public field
  mark is *not* an error).

* The interface codec is exercised on the exact `Out.sym` example written out in
  §7, on a round trip covering all four item kinds, and on four malformed inputs
  (declaration before `MODULE`, duplicate `MODULE`, `CONST` without `=`, `PROC`
  without `)`, and an inline `PROCEDURE (…)` parameter type).

* The type algebra (`src/types.rs`, 12 tests) is where the report's
  wording is densest, so each test is named after the rule it pins: pointer
  identity and extension following the base, record extension being reflexive,
  transitive and one-way, the four §9.1 assignment exceptions, the §10.1
  parameter modes (including `VAR` requiring *identical* while a record formal
  may be a *base* of the actual's), the operand rules of §8.2.1-§8.2.4, and the
  §10.2 signatures.

* The constant folder (`src/const_eval.rs`, 7 tests) is checked twice: once
  against hand-written expectations, and once against the C back end's own
  helpers via `golden/constexpr.tsv` — 52 expressions whose values the C
  side computes with `rt_div`, `rt_mod`, `rt_lsl`, `rt_asr`, `rt_ror`, `rt_set`,
  `rt_in`, `rt_floor`, `rt_flt`, `rt_ord`. **The Rust folder and the emitted C
  agree on every one**, which is the design's claim in §8.3, executed. Building
  this table also caught two of my own arithmetic slips in the *test
  expectations* (the precedence of a leading sign, and a symmetric-difference
  set value), which is the other reason to have two independent implementations.

* The reclassification tests are what turn decision D2 and D3 from "argued" into
  "executed": they assert the four guard/call shapes, the module-qualification
  fold, that `t.f` stays a field select when `t` is not a module, that a module
  using every report construct ends with no unresolved `Selector::Args`, and that
  a record `CASE` label which is neither a declared type nor a module is
  rejected with a span.

* Zero LALRPOP conflicts with both constructions: the default lane-table LR(1)
  (`cargo test`) and the canonical LR(1) builder
  (`std::env::set_var("LALRPOP_LANE_TABLE", "disabled")` in `build.rs`).
  Conflicts are hard errors in LALRPOP, so a successful build *is* the evidence.
* The two conflict transcripts quoted in D3 and D4 were produced by the
  canonical LR(1) builder, which prints the competing parses.
* `src/lib.rs::tests::parses_kitchen_sink` parses one module containing
  every construct of the report: all four declaration sections, all six type
  forms, all seven statement forms, sets with ranges, typed and scalar `CASE`,
  guards, `IS`, strings, hex integers, character constants, `VAR`/value/open-array
  parameters, nested declarations, and imports with aliasing.
* `tests::reports_syntax_errors_without_panicking` shows that both a syntax error
  and an unterminated comment produce a parse error rather than a panic.
* `golden` (`bash golden/run.sh`) hand-writes the C that §9.6–9.8
  prescribe for the report's own `log2`, `WriteInt` and `ReadInt`, for a
  `WHILE … ELSIF` loop, five `FOR` shapes checked against the report's
  desugaring, an `IF`/`ELSIF`/`ELSE` chain, a `REPEAT`, a range `CASE` with a
  body-less default, and a two-module program with `Out__init`/`Out__load`, a
  command table and exported prototypes. **66 cases pass**, and the whole suite
  is run a second time under `-fsanitize=address,undefined
  -fno-sanitize-recover=all` with leak detection on, so the bounded comparisons
  and copies are proved not to read past their operands. It found three
  corrections, all folded back into this document: R12 (command arity), R13
  (`BY = 0`) and R14 (unterminated `CHAR` arrays, which turned `CHAR` array
  comparison into a bounded `rt_charcmp` — itself now pinned by 12 cases,
  including `charcmp-adjacent-struct-sentinel`, where an unbounded compare reads
  the neighbouring byte and answers the wrong way).
* **`e2e` — the whole compiler, end to end.** Five Oberon modules (the
  report's own `Out`, a hello-world, a language torture program and a
  deliberately trapping one, plus `Texts`/`Oberon` as hand-written runtime
  fixtures in the emitted shape) are compiled by this repository's compiler into
  C, that C is
  compiled by the host `cc` under `-std=c11 -O2 -fwrapv -fno-strict-aliasing
  -Wall -Wextra -Werror`, the program runs, and its output is compared with
  `expected.txt` — which is computed from the report's semantics, not copied from
  a run. A second module deliberately indexes out of bounds to show that a trap
  names the source offset.

  The driver is exercised there too: `obc build`/`obc run` resolve the import
  graph (`Torture` → `Out` → `Texts`, `Oberon`), generate the loader, invoke the C
  compiler, and the resulting program's output is compared with `expected.txt`;
  `obc check` on a broken module must produce a located diagnostic. `obc run
  Hello.mod` is the hello-world of the whole system: one command, a runnable
  program.

  This is where most of the design's mistakes were found, and it is worth listing
  them because each one is a rule that looked obviously right when written down:

  *emitter*

  1. user procedures were called with no arguments (the emitter had no symbol for
     them, so `call_parts` fell through to an empty list);
  2. every declaration resolved to `TypeId(0)`, because type *expressions* were
     not recorded by span — and, later, because a record body is built on a path
     that bypasses `type_expr`;
  3. `()` instead of `(void)` for an empty parameter list;
  4. the command table was emitted before the prototypes it names;
  5. the typedefs were emitted in both the header and the translation unit, and
     the header had no include guard;
  6. an imported type was re-declared locally, giving two incompatible C types
     for one Oberon type (now the defining module's header wins);
  7. a module-qualified call lost its name and arguments;
  8. `ORD("0")` cast a string literal to an integer;
  9. a procedure-typed variable was declared `int32_t (*)(int32_t) f;`, which is
     not a declaration at all;
  10. a `VAR` parameter was used without the deref its C type requires;
  11. an open-array parameter was indexed with the fixed-array `.e[]` form, and
      `A[i, j]` descended one level instead of two;
  12. **set arithmetic was emitted as C arithmetic** — `s * t` became a 32-bit
      multiply rather than an intersection, a silent miscompile;
  13. an inherited field needed its `.base.` path;
  14. a derived record was given the *base* type's descriptor, and every record
      declared its own `desc` field, so a derived record's first word was NULL
      and the type `CASE` segfaulted — the layout rule is that only the base-most
      record of a chain declares the descriptor;
  15. a nested procedure was emitted *inside* its parent's body, so the parent's
      body never ran;
  16. the static-link walk was off by one (`sl` already points at the parent's
      frame);
  17. a type `CASE` could emit two `default:` labels;
  18. a variable of record type was never given its descriptor, so a `VAR` record
      parameter carried no dynamic type.

  *checker and grammar*

  19. `IS` reused its operand's span, so the test's own `BOOLEAN` type hid the
      operand's type in the emitter's side table (fixed in the grammar);
  20. a `VAR` record parameter lost the guard/`CASE` flag when the read-only rules
      were split;
  21. `A[i, j]` was typed one level down instead of two;
  22. an imported variable was rejected as a `VAR` actual — the report's own
      `Out` passes `Oberon.Log` to `Texts.Append(VAR w: Writer; …)`;
  23. an interface rendering of `ARRAY OF CHAR` was not parseable, because a
      formal type is `{ARRAY OF} qualident` rather than an ordinary type;
  24. a record field's `*` (report §6.3's *public* field) was rejected;
  25. `rt_charcmp`'s tail rule made a filled array and an equal-content
      terminated one compare unequal, which contradicted `strcmp`.

  A second round of e2e coverage — type guards in both spellings, type `CASE`
  arm narrowing, `INCL`/`EXCL`, `PACK`/`UNPK`, a record value parameter, `CHAR`
  `CASE` labels, an open array of `CHAR`, and `NEW` on a *derived* pointer's type
  — found five more:

  26. **an assignment between two pointer types where the source extends the
      destination was rejected.** The §9.1 extension exception had been applied
      to records only, although §6.4 gives pointers the same relation — so
      `p := q` with `q: POINTER TO Derived` and `p: POINTER TO Base` failed, which
      is ordinary Oberon;
  27. the *checker* did not give the case variable the label's type inside a type
      `CASE` arm, so a derived field was unreachable there (report §9.5). The
      emitter rebinds that name and the checker did not — the two halves of one
      rule, drifted apart;
  28. a cast followed by a member access needs its own parentheses
      (`((T *)p)->f`, not `(T *)p->f`, which C reads as `(T *)(p->f)`); two sites
      emitted the second form;
  29. a *record* value parameter was declared **by value** in the callee while the
      caller passed a pointer to the copy it had made — the two halves of §9.7's
      convention, again drifted apart;
  30. the caller's copy for a record value parameter was initialised with a plain
      C assignment, which does not accept a derived actual for a base-typed
      formal.


  Each of those is now pinned by a test: the emitter's by the e2e output, the
  checker's by the catalogue cases, and the runtime's by `golden`.

* **The review round.** A full read of the code (every file under ``) found
  34 defects, 21 of them high. They are fixed and pinned; the ones worth naming
  are the silent-miscompile class, because each of them compiled and ran:
  constants were re-folded by the emitter with its own partial rules
  (`CONST B = A + 1` emitted `0 + 1`, `7 DIV 3` emitted `0`); a recursive record
  type made the compiler recurse until its stack died; a `CASE` with a range or a
  multi-label arm emitted unbalanced braces; an up-cast `b := d` copied the
  *source's* descriptor, so a base variable could advertise a derived dynamic
  type and a type `CASE` then read past its allocation; a failing guard written
  with the pointer spelling performed no check at all; a module-qualified read
  dropped its selectors (`M.r.f` became `M__r`); a call through a procedure-valued
  field called the wrong function; forwarding an open array took the address of
  the parameter; integer `/` used C's division while the folder used `DIV`;
  `CHAR` order depended on whether C's `char` is signed; `PACK` modified its
  exponent and `UNPK` was `PACK`; procedure locals and frame locals were never given
  descriptors; and a record-valued field declared by a *base* type was skipped by
  the descriptor walk (which then reset the outer descriptor). Two of the
  findings were in the *design*, not the code: the documented ABI put the
  descriptor "in front of the payload" (it is the base-most record's first word)
  and claimed every emitted identifier was prefixed (locals and fields are not,
  except for C keywords).
* **The fixture that had drifted.** `golden` used to carry a private copy of
  the runtime helpers, and its `rt_charcmp` had already diverged from the emitted
  one — exactly the failure mode the review predicted. `rt.h` now *includes*
  `src/oberon_h.inc`, so those vectors test the runtime the compiler ships; the
  divergence showed up immediately as two failing cases, which is how it was
  found.
* `csmoke` (`bash csmoke/run.sh`) covers the C rules of §9.5, §9.3
  and §9.7: `DIV`/`MOD` by brute force, set literal and operator semantics,
  the `FOR` header's iteration counts, `CHAR` array copy/compare, descriptor
  chain walks with a guard trap, shifts, open-array `LEN`, value vs `VAR`
  parameter visibility, `FLOOR`/`FLT`/`ORD`; plus the three rules that had no runtime evidence before:
nested procedures through a hidden static link (three levels deep), the
descriptor-`id` type `CASE` with a trapping `default`, and caller-side copying
for `ARRAY` value parameters. 67 cases pass under
  `gcc -std=c11 -O2 -fwrapv -Wall -Wextra -Werror`; the one FAIL line is the
  documented `ROR` spec bug (R11), not a mapping failure.

## 15. Storage reclamation: what the research settles **[implemented; PLAN.md W10.2. The runs that execute each claim are named where it is made — `e2e`'s collector rows, `fuzz`'s stress blocks, and the descriptor shapes they compile]**

Oberon-07 has `NEW` and no `DISPOSE` (report §6.4), so every program this compiler
builds leaks until it dies of it. The language's own shape makes the fix cheap: a
pointer must refer to a *record* (§6.4, enforced by the checker), every heap object
begins with its `ObDesc` (§9.3), and `NEW`'s allocation goes through one runtime
entry point (W7.4). What follows is decided from the sources, not assumed.

*Wirth, Compiler Construction, §14.2 (pp. 86-87)* asks the compiler designer's
question — which data the collector needs — and answers it with exactly three
compile-time facts: the addresses of all declared pointer variables, the offsets of
all pointer fields in dynamically allocated records, and the size of every allocated
variable; `NEW` hands the second and third to the collector as a *type descriptor*
attached to the block, and the object file carries the first as a list copied at
load time. The same pages note the restriction this design already enforces: a
pointer may only refer to a record, so descriptors are needed for records alone.

*Mogensen, Introduction to Compiler Design, §12.2 (p. 261)* states the approximation
and the root set: an object is reclaimed when it can no longer be *reached*, and the
root set is "all variables allocated in registers, spilled on the stack, saved across
procedure calls, or allocated globally".

*Sanyal and Khedker, The Compiler Design Handbook, ch. 6* gives the method taxonomy
(reference counting, mark-sweep, mark-compact, copying, generational, incremental
and concurrent).

From those three, the decisions:

1. **Mark-sweep, non-moving.** The emitter addresses heap objects through raw
   pointers — every `p.f`, every descriptor read — so a moving collector (copying,
   mark-compact) would have to find and rewrite every pointer, including the ones in
   the frames the C back end does not describe. A reclaimer is what the language
   needs and what this back end can have.
2. **The mark is precise, driven by the descriptor.** The descriptor gains the
   pointer-field offsets Wirth's scheme requires. The emitter already walks exactly
   those fields — `desc_init` installs the nested descriptors — so the map costs one
   more thing to emit per record type, and objects whose type has no pointer fields
   need no map at all.
3. **The stack walk is conservative, for now.** The C back end cannot enumerate the
   roots in registers and spilled slots (Mogensen's root set), and it has no stack
   maps; a value that merely *looks* like a pointer is therefore treated as one. That
   is safe in the only direction that matters: it can retain garbage, never free a
   live object. Interior pointers are treated as pointing at their object. The
   Cranelift back end (W9) *does* produce stack maps, so the same collector can take
   a precise stack later without changing the heap.
4. **The roots are stated.** Every module-level variable of pointer type (a list the
   loader registers, as Wirth's object file does), the conservative stack and
   register scan, and the live state of the command table. The hidden open-array
   *length* is an `int32_t` that may look like a pointer — a false retention, not a
   lost object.
5. **Collection happens before `NEW` gives up**, so the report's
   `NEW`-yields-`NIL` (§6.4) keeps its meaning with a bounded heap, and so that
   `OB_ALLOC_FAIL_AT` stays a test hook rather than the only way an allocation can
   fail. A collection that frees nothing ends the attempt rather than looping.
6. **Two things it must never do**: free a live object — the generator's self-checks
   are the oracle, and the corpus is what will say whether a reclaimer is correct —
   and move one.

7. **The allocation list is a header in front of the payload.** `rt_new`
   allocates `sizeof(ObBlock) + sizeof(T)`, links the block into one list, and
   returns the payload — so the object's first word is still its descriptor and
   §9.3's layout rule is untouched. The header carries the next block, the
   payload's size and the mark, which is what the sweep needs; and a size is also
   what decides whether a word points *into* a block rather than at its first
   byte, which is the interior-pointer case a separate table would have to answer
   with the same range test anyway while staying in step with the heap by hand.
   It costs one `ObBlock` (24 bytes) per object, and `OB_GC_LIMIT` counts it, so
   a bounded heap is bounded in the bytes it really takes.
8. **The trigger besides a failing allocation is a byte budget,
   `OB_GC_LIMIT=n`.** The heap's live bytes are the headers plus the payloads;
   when the next allocation would take them past `n`, the collector runs, and
   `NEW` yields `NIL` if it still would afterwards — which is the report's
   failure case (§6.4) reached without a failing `malloc`, and the one thing that
   makes a bounded heap testable at all. `malloc` failing collects once and
   retries once, so no path loops. Item 5's "ends the attempt"
   is therefore two `NIL` returns and no loop: a collection that frees nothing
   leaves the budget over, and the attempt ends. Evidence: `e2e`'s
   `gc-runs-in-a-bounded-heap-limit` collects 20000 allocations into a
   4096-byte heap and completes, and `a-live-set-over-the-limit-yields-nil`
   reaches the `NIL` with a live chain longer than the limit and then counts it
   back intact.
9. **`OB_GC_STRESS=1` collects before every allocation**, so a suite sees the
   collector run without arranging a heap small enough to fail. It changes no
   answer because the mark is exact in the direction that matters: what is
   reclaimed is what no root could reach — a module variable, a pointer inside a
   live object or a record variable, or a word in the stack that points into the
   heap — and nothing moves, so a pointer a program still holds is still valid.
   The one observable difference is when an *address* is reused, and that is
   `malloc`'s to decide in every build, collector or not. Evidence: the fuzz
   corpus, whose probe keeps a chain alive per root kind and checks it after
   churn, passes under `OB_GC_STRESS=1`; `e2e`'s three `gc-runs-in-a-bounded-heap-*`
   rows are the same program with the collector stressed, limit-triggered and
   idle, and all three end in the same value.
10. **The descriptor gains the map, and only records that need one.** `ObDesc`
    is now `{base, id, name, fields, nfields}`, and an `ObFld` is
    `{off, stride, n, desc}`: `desc == NULL` is `n` pointer slots `stride` bytes
    apart (one slot, or an array of pointers), and `desc` non-NULL is `n` records
    of that type beginning there — a record field, or an `ARRAY OF RECORD` —
    whose own map takes over. A type that reaches no pointer has `fields == NULL`
    and `nfields == 0`, so marking an object of such a type is one test rather
    than a walk, and a program that keeps no pointers registers no root. The map
    is emitted by the same walk `desc_init` uses to give nested records their
    descriptors, and its offsets are `offsetof` expressions, so the map follows
    the C layout the back end does not compute for itself. Because a base is a
    derived record's first member, a base's pointer fields arrive through one
    more `offsetof` term per level. The one hand-written descriptor literal in
    the tree — `e2e/Texts.c`, a fixture in the emitted shape — lists the two new
    fields, which is not collateral: `-Wextra`'s `-Wmissing-field-initializers`
    would reject the old literal under the e2e build's `-Werror`.
11. **The roots are the module variables whose storage can reach the heap.**
    `__init` registers each one — before its body can allocate — with
    `ob_gc_root_region(&var, map, n)`: a pointer variable is one slot at offset
    0, a record variable is its own type's map, and an `ARRAY OF RECORD` is a run
    of them. The command table needs no entry of its own: it holds names and code
    addresses, and what it can reach it reaches through the module variables that
    are already roots. Everything else is item 3's conservative walk. Evidence:
    `e2e`'s `gc-roots-hold-their-objects` (a module-level `ARRAY OF POINTER`, and
    a record type whose map is emitted in another module's translation unit),
    `gc-an-inherited-pointer-field-is-a-root` (a field the *base* of the object's
    type declares, which the derived type's map has to reach one level in), and
    `fuzz`'s probe, which holds a chain in a local, in a module-level pointer
    variable, and in a module-level record both as one field and as a run.

Two things only a run could settle, and both are in the runtime as written:

* **A sanitized build needs two concessions from the collector, and they are part
  of the walk's contract rather than a test-only workaround.** A conservative walk
  is only correct if it may read the region it walks: ASan keeps locals in a "fake
  stack", which a walk of the stack cannot see, and poisons the space around the
  variables that are on the stack, which that read would trip over. The runtime
  therefore turns `detect_stack_use_after_return` off in a constructor that runs
  before the one capturing the stack top, and unpoisons the region it is about to
  read; both are no-ops without a sanitizer, and neither changes what the
  collector does. A program built with ASan and the collector on is then correct
  for the same reason it is without one. This is worth the six lines: the value
  checks alone did not notice a live object being freed (freed memory reads
  correctly until it is reused), and ASan is what found a defect in the walk while
  this was being written. The stress blocks of `fuzz/run.sh` therefore build with
  the sanitizers whether or not `SAN=1` is set.
* **The walk covers the frames from the collector's own up to a base captured
  before `main`** (a constructor's frame is above every frame the program itself
  will use). A host that kept a heap pointer only in its own `main` frame would
  be outside it; the generated `main` calls the module loaders and the command
  runner and holds no Oberon variable, so nothing is. The setjmp in the same
  helper is what covers the *registers*: a value a callee-saved register carries
  across the call to `rt_new` is nowhere in the stack until something spills it,
  and `setjmp` puts it in the frame the walk is already reading. Completing this
  precisely is what W9's stack maps are for, and it would change nothing else
  here.
