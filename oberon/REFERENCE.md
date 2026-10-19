# Command-line and language reference

The repository builds three tools: `obc` compiles and runs modules, `oberon-front` exposes parsing and single-module checking, and `obgen` generates compiler test programs. The main compiler accepts Oberon-07 source plus the extensions below. See `DESIGN.md` for the language rules and the reasons behind the compiler's design.

## `obc`

```text
obc <check|emit|build|run> FILE.mod [options]
```

`FILE.mod` is the root module. Imports are resolved depth-first. The compiler looks for `M.mod`; a hand-written C source file `M.c` can be used instead when an `M.sym` interface exists. `build` and `run` also require `M.h` beside that C file. `SYSTEM` is built in.

| Mode | Effect |
|---|---|
| `check` | Parse and type-check the root and source-module imports against available interfaces. Print diagnostics. Does not link or run a program. |
| `emit` | Generate per-module output and runtime support without linking. |
| `build` | Generate and link an executable without running it. |
| `run` | Generate, link, and run an executable. Module initialization runs before any requested command. |

### Options

| Option | Meaning and default |
|---|---|
| `-I DIR` | Add an interface/source search directory. Repeatable. With no `-I`, the root module's parent directory is searched (`.` for a bare filename). If supplied, the `-I` directories replace that default. `--out` is appended last, so earlier search paths take precedence. |
| `--out DIR` | Generated-file directory. Defaults to `.` for `check` and `emit`. `build` and `run` use a temporary directory when this option is omitted. |
| `-o EXE` | Executable path for `build` or `run`. Default: `./<Module>` for `build`, `<out>/<Module>` for `run`. |
| `--cc CC` | C compiler used for linking. Default: the value of `$CC`, or `cc` when `$CC` is unset. |
| `--cflags "FLAGS"` | Append flags to the compiler command. The value is split on whitespace. |
| `--cflag ARG` | Append one compiler argument verbatim. Repeatable. Pass an argument as-is when it contains spaces. |
| `--command M.P` | Dispatch exported, parameterless procedure `P` from module `M` after module initialization. Applies to `run`; for `build`, the executable includes the same command dispatch. |
| `--keep` | Keep the temporary directory used by `build` or `run` when `--out` is omitted. |
| `--line-directives` | Emit C line directives that map generated statements to Oberon source lines. Valid for `emit`, `build`, and `run` with the C backend. Invalid for `check` and `--backend=clif`. |
| `--ferror-limit N` | Print at most `N` errors. Default: `20`. Use `0` for no limit. |
| `-Ochecks` | Enable all runtime checks. This is the default. |
| `-Ochecks=LIST` | Select comma-separated checks from `index,nil,guard,case`. Also accepts `all`/`y` and `none`/`n`. |
| `-Odialect=report\|lower` | Select reserved-word case handling. `report` is the default. `lower` accepts reserved words in any ASCII letter case. Identifiers and predeclared names remain case-sensitive. |
| `-Oopt=LEVEL` | Cranelift optimization level: `speed` (default), `none`, or `speed_and_size` (also spelled `speed-and-size`). Has no effect on the C backend. |
| `--backend=c\|clif` | Select the backend. `c` (default) emits C source and headers. `clif` emits one object file per compiled module. Both are linked by the selected C compiler. |
| `--layout` | Print a C ABI layout table for the root module's types and stop before linking. |

The C backend starts with `-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror -Wno-overflow`. `--cflags` and `--cflag` add arguments after those defaults. `-Oopt` is for Cranelift, not for the C compiler.

The driver writes shared runtime support and generated module files under `--out`. In `check`, it also writes these files before returning, despite a driver comment that says otherwise. Use a scratch `--out` directory to keep the files out of the source tree. `check` does not invoke the C compiler or run the program.

Examples:

```sh
obc check app.mod -I lib
obc emit app.mod --out build
obc build app.mod -o app
obc run app.mod --command App.Start
```

## `oberon-front`

```text
oberon-front [FILE.mod|-] [options]
```

With no file, or with `-`, it reads standard input. With only a source file it parses and prints the procedure-oriented AST. Use `obc check` for checking an import graph.

| Option | Meaning |
|---|---|
| `-c`, `--check` | Check one module and print diagnostics instead of the AST. |
| `--emit-c DIR` | Check and emit `<Module>.c`, `<Module>.h`, the runtime files, and a `.sym` interface when available. `DIR` defaults to `.`. Imported interfaces are read from the source directory and `-I` directories. |
| `-I DIR` | Add an interface search directory for `--emit-c`. Repeatable. |
| `--line-directives` | Add C line directives when used with `--emit-c`. |
| `--ferror-limit N` | Limit printed errors; default `20`, `0` means no limit. |
| `-Odialect=report\|lower` | Select report or case-folded reserved words, as for `obc`. |

## `obgen`

`obgen` generates self-checking modules and runs generated programs through `obc`.

| Option | Meaning and default |
|---|---|
| `--seeds N` | Number of consecutive seeds to run. Default: `25`. |
| `--base N` | First seed. Default: `1`. |
| `--seed-list FILE` | Run the seeds listed in a file instead of the consecutive range. |
| `--obc PATH` | Compiler executable; default `target/debug/obc`, relative to the current directory. |
| `--work DIR` | Work directory. Default: `<system-temp-dir>/obgen`. |
| `--print` | Print the generated module for `--base` and stop. |
| `--explain NAME` | Print the generated value trace for module variable `NAME` in seed `--base` and stop. |
| `--per-statement NAME` | Print the module for seed `--base` with a value check for `NAME` after each statement, once that variable has been assigned. |
| `--adversarial` | Generate a program whose final action deliberately traps. |
| `--lower` | Render reserved words with folded case and run the compiler in the `lower` dialect. |
| `--backend=c\|clif` | Backend passed to `obc`. Default: `c`. |
| `--cflag ARG` | Compiler argument passed through to `obc`. Repeatable. |
| `--keep` | Keep generated work files after a run. |
| `--compiler-subset=source\|selfcheck\|harness` | Reuse obgen's deterministic seed RNG to generate a restricted INTEGER-procedure test, a self-checking Oberon module for the C/Cranelift backends, or an independent C oracle for the standalone Rust compiler. Use `--base SEED`. |

`--print`, `--explain`, and `--per-statement` use the seed selected by `--base`. Normal runs use `--seed-list` or the `--base`/`--seeds` range.

### Shared compiler-subset differential seeds

The new standalone Rust compiler supports a deliberately narrower subset than
`obc`'s full Oberon-07 language. The existing full `obgen` suite remains
unchanged; `--compiler-subset=...` creates deterministic procedures with only
value INTEGER parameters and locals, 32-bit arithmetic, comparisons,
IF/ELSIF/ELSE, WHILE, assignments and RETURN.

```sh
obgen --base 7 --compiler-subset=source > Fuzz.mod
obgen --base 7 --compiler-subset=selfcheck > Fuzz_reference.mod
obgen --base 7 --compiler-subset=harness > harness.c
```

All three outputs for a seed share the same `Fuzz.Probe` function. The
`selfcheck` variant adds module-initialization assertions for 44 input
triples, suitable for `obc build ... --backend=c` and
`--backend=clif`. The C harness checks the same independently interpreted
values against the new compiler's `Fuzz__Probe` assembly entry point.
The test values include signed-32-bit boundaries and wrapping arithmetic.
Any seed reproduces the same procedure and expected values.

This mode is intentionally separate from the full fuzzer's module-variable,
call, recursion, pointer, heap, trap and sanitizer coverage. Those constructs
will only be included in cross-backend testing as the new compiler supports
them.

## Supported Oberon language

This describes the implemented source language, not every extension found in Oberon-2 or later dialects. Reserved words are uppercase in the default `report` dialect. `-Odialect=lower` folds reserved-word case only. Identifiers and predeclared names remain case-sensitive.

### Source form and lexical rules

A source file contains one module, with an optional import list, declarations, an optional module body, and a matching end name:

```oberon
MODULE Example;
IMPORT O := Out;
CONST Limit = 10;
TYPE Index = INTEGER;
VAR i: Index;

PROCEDURE Start*;
BEGIN
  i := 0
END Start;

BEGIN
  O.Int(i, 4)
END Example.
```

Declaration sections appear in this order: `CONST`, `TYPE`, `VAR`, then procedures. `*` exports a declaration. Identifiers start with an ASCII letter and continue with ASCII letters or digits. `_` is not allowed. Comments use nested `(* ... *)`. Strings use double quotes and have no escape sequences. A one-character string can be used as a `CHAR`. Numeric forms include decimal and hexadecimal integers (`100H`), character constants (`22X`), and real numbers with an exponent (`1.5E3`). The lexer distinguishes the range `1..5` from a real literal.

### Declarations, types, and procedures

The predefined basic types are `BOOLEAN`, `CHAR`, `INTEGER`, `REAL`, `BYTE`, and `SET`. `INTEGER` and `REAL` are 32-bit. `BYTE` and `CHAR` are 8-bit. `SET` is 32-bit. `BOOLEAN` is represented by the values `0` and `1`. `BYTE` and `INTEGER` are assignment-compatible with each other. There is no implicit conversion from `INTEGER` to `REAL`.

The language supports constants, type declarations, fixed-size (including multidimensional) arrays, records with record extension, pointers, and procedure types. Only formal parameters can use `ARRAY OF T`. `LEN` takes one argument. Procedures can nest. They have value or `VAR` parameters and may return a value. Exported, parameterless procedures can be module commands.

### Statements and expressions

Supported statements include assignment, procedure calls, `IF`/`ELSIF`/`ELSE`, and `CASE`. Loop statements are `WHILE`/`ELSIF`, `REPEAT`/`UNTIL`, and `FOR`/`TO`/`BY`. `RETURN` and `EXIT` are also supported. `CASE` supports value labels and ranges, type cases, and an `ELSE` arm. `IS` tests types. Pointer and record guards are supported.

Expressions include integer, real, character and string literals; `NIL`, `TRUE`, and `FALSE`; set constructors; designators and procedure calls; unary `+`, `-`, and `~`; arithmetic `+`, `-`, `*`, `/`, `DIV`, and `MOD`; Boolean `&` and `OR`; and relations `=`, `#`, `<`, `<=`, `>`, `>=`, and `IN`. `IS` is the type-test operator. Relation chains are not supported.

Predeclared functions: `ABS`, `ODD`, `LEN`, `LSL`, `ASR`, `ROR`, `FLOOR`, `FLT`, `ORD`, and `CHR`. Predeclared procedures: `INC`, `DEC`, `INCL`, `EXCL`, `NEW`, `ASSERT`, `PACK`, and `UNPK`. The `MIN(T)` and `MAX(T)` functions are supported for `INTEGER`, `BYTE`, and `CHAR`.

`SYSTEM` is a built-in imported module. Its portable entries are `ADR`, `SIZE`, `BIT`, `GET`, `PUT`, and `COPY`. These operations access memory without bounds or type checks. Optional, system-specific entries such as `VAL`, `ADC`, `SBC`, `UML`, `COND`, and `LED` are not implemented.

### Extensions and exclusions

The default dialect includes four additive extensions: `CASE ... ELSE`, `EXIT`, early `RETURN`, and `MIN(T)`/`MAX(T)`. The optional `lower` dialect changes reserved-word case handling.

The compiler does not support `WITH`, `LOOP`, type-bound procedures, `SHORTINT`, `LONGINT`, `LONGREAL`, numeric widening, `HALT`, `ROL`, `LEN(array, dimension)`, `NEW(pointer, lengths...)`, runtime-sized or multidimensional open arrays, or open arrays outside formal parameters. It also rejects `_` in identifiers, `//` comments, single-quoted strings, omitted module/procedure end names, and conditional compilation.

For semantic details and design evidence, see [DESIGN.md](DESIGN.md) and its language and backend sections.
