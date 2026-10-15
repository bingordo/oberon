#!/usr/bin/env bash
# End-to-end: compile Oberon-07 modules to C with the compiler in this
# repository, compile that C with the host cc, run it, and compare with the
# expected output. Texts and Oberon are hand-written runtime fixtures in the
# shape the emitter produces, so the report's own Out module compiles unchanged.
set -e
cd -- "$(dirname -- "$0")"

OB=${OB:-../target/debug/oberon-front}
CC=${CC:-gcc}
# The build line of DESIGN 9.1, plus -Werror so the emitted C has to be clean.
CFLAGS="-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror"

pass=0
fail=0
check() { # name expected actual
    if [ "$2" = "$3" ]; then
        echo "PASS $1"
        pass=$((pass + 1))
    else
        echo "FAIL $1: expected [$2] got [$3]"
        fail=$((fail + 1))
    fi
}

OUT=$(mktemp -d)
DRV=$(mktemp -d)
trap 'rm -rf "$OUT" "$DRV"' EXIT

# 1. Interface files are part of the build: the compiler writes them and reads
#    them back for the importing module.
cp Texts.sym Oberon.sym Texts.h Texts.c Oberon.h Oberon.c "$OUT"/
"$OB" Out.mod --emit-c "$OUT" -I "$OUT" >/dev/null
check "compiler-writes-an-interface" "MODULE Out" "$(head -1 "$OUT/Out.sym")"
check "interface-records-its-imports" "IMPORT Texts, Oberon" "$(sed -n '2p' "$OUT/Out.sym")"
check "interface-lists-the-exported-procedures" "1" \
    "$(grep -c '^PROC \(Write\|WriteInt\|WriteLn\)$' "$OUT/Out.sym")"

"$OB" Torture.mod --emit-c "$OUT" -I "$OUT" >/dev/null

# 2. The emitted C compiles as C11 with no warnings.
if $CC $CFLAGS -I "$OUT" -o "$OUT/torture" "$OUT"/Torture.c "$OUT"/Out.c "$OUT"/Texts.c \
        "$OUT"/Oberon.c "$OUT"/oberon_runtime.c main.c 2>"$OUT/cc.log"; then
    echo "PASS emitted-c-compiles-cleanly"
    pass=$((pass + 1))
else
    echo "FAIL emitted-c-compiles-cleanly:"
    sed -n '1,20p' "$OUT/cc.log"
    fail=$((fail + 1))
fi

# 3. It runs, and it prints what the report's semantics say it should.
"$OUT/torture" > "$OUT/got.txt"
check "torture-program-output" "$(cat expected.txt)" "$(cat "$OUT/got.txt")"

# 4. A trap is locatable: the bounds-checked program reports the source offset.
"$OB" Trap.mod --emit-c "$OUT" -I "$OUT" >/dev/null
if ! $CC $CFLAGS -I "$OUT" -o "$OUT/trap" "$OUT"/Trap.c "$OUT"/Texts.c "$OUT"/Oberon.c \
        "$OUT"/oberon_runtime.c main_trap.c 2>"$OUT/trap.cc.log"; then
    echo "FAIL emitted-trap-program-compiles:"
    sed -n '1,20p' "$OUT/trap.cc.log"
    fail=$((fail + 1))
fi
# A trap aborts the process, which the shell would report as a job message;
# running it through `sh -c` keeps the suite's output to PASS/FAIL lines.
sh -c '"$0" >/dev/null 2>"$1"' "$OUT/trap" "$OUT/trap.err" || true
check "trap-reports-a-source-offset" "1" \
    "$(grep -c 'index out of range (at byte' "$OUT/trap.err")"

# 5. Hello world: a module whose body greets on load, plus a command.
"$OB" Hello.mod --emit-c "$OUT" -I "$OUT" >/dev/null
if $CC $CFLAGS -I "$OUT" -o "$OUT/hello" "$OUT"/Hello.c "$OUT"/Texts.c "$OUT"/oberon_runtime.c \
        main_hello.c 2>"$OUT/hello.cc.log"; then
    echo "PASS hello-world-compiles"
    pass=$((pass + 1))
else
    echo "FAIL hello-world-compiles:"
    sed -n '1,20p' "$OUT/hello.cc.log"
    fail=$((fail + 1))
fi
"$OUT/hello" > "$OUT/hello.got"
check "hello-world-runs" "$(cat hello.expected.txt)" "$(cat "$OUT/hello.got")"

# 6. The driver: source in, runnable program out, with the import graph
#    resolved and the C compiler invoked for us.
OBC=${OBC:-../target/debug/obc}

if "$OBC" build Hello.mod -I . --out "$DRV" -o "$DRV/hello" 2>"$DRV/build.log"; then
    echo "PASS driver-builds-a-program"
    pass=$((pass + 1))
else
    echo "FAIL driver-builds-a-program:"
    sed -n '1,20p' "$DRV/build.log"
    fail=$((fail + 1))
fi
check "driver-built-program-runs" "$(cat hello.expected.txt)" "$("$DRV/hello")"

# The driver resolves imports recursively: Torture -> Out -> Texts, Oberon.
if "$OBC" run Torture.mod -I . --command Torture.Run >"$DRV/torture.got" 2>"$DRV/run.log"; then
    echo "PASS driver-runs-a-command"
    pass=$((pass + 1))
else
    echo "FAIL driver-runs-a-command:"
    sed -n '1,20p' "$DRV/run.log"
    fail=$((fail + 1))
fi
check "driver-output" "$(cat expected.txt)" "$(cat "$DRV/torture.got")"

# A module that does not check is rejected with a located diagnostic: the file,
# the line and column, the source line, a caret under the span, and the reason.
printf 'MODULE Bad;\n  VAR x: INTEGER;\nBEGIN\n  x := NIL\nEND Bad.\n' > "$DRV/Bad.mod"
"$OBC" check "$DRV/Bad.mod" -I . >"$DRV/bad.out" 2>&1 || true
check "driver-locates-a-semantic-error" "1" \
    "$(grep -c '^.*Bad.mod:4:3: error: cannot assign NIL to an INTEGER' "$DRV/bad.out")"
check "driver-shows-the-source-line-under-a-caret" "1" \
    "$(grep -c '^4 |   x := NIL$' "$DRV/bad.out")"
check "driver-explains-the-rule" "1" \
    "$(grep -c '= note:.*pointer or a procedure variable' "$DRV/bad.out")"

# The parser recovers at statement level, so *one* run reports both the syntax
# error and the type errors after it (the file has three mistakes; the run
# reports all three, not just the first).
printf 'MODULE Mix;\n  VAR x: INTEGER;\nBEGIN\n  x := ;\n  y := 2;\n  z := 3\nEND Mix.\n' > "$DRV/Mix.mod"
"$OBC" check "$DRV/Mix.mod" -I . >"$DRV/mix.out" 2>&1 || true
check "driver-reports-syntax-and-semantic-errors-together" "3" \
    "$(grep -c 'error:' "$DRV/mix.out")"
check "driver-recovers-past-the-syntax-error" "1" \
    "$(grep -c 'Mix.mod:5:3: error: `y` is not declared' "$DRV/mix.out")"

# A module that only *warns* still reports it, and still exits 0: warnings are
# not errors, but they must not be invisible either.
printf 'MODULE Warn;\n  IMPORT Texts;\nBEGIN\nEND Warn.\n' > "$DRV/Warn.mod"
warn_status=0
"$OBC" check "$DRV/Warn.mod" -I . --out "$DRV" >"$DRV/warn.out" 2>&1 || warn_status=$?
check "clean-module-shows-its-warning" "1" \
    "$(grep -c 'warning: import `Texts` is never used' "$DRV/warn.out")"
check "a-warning-does-not-fail-the-build" "0" "$warn_status"

# Two syntax errors in one body are both reported.
printf 'MODULE Two;\n  VAR x, y: INTEGER;\nBEGIN\n  x := ;\n  y := ;\n  x := 1\nEND Two.\n' > "$DRV/Two.mod"
"$OBC" check "$DRV/Two.mod" -I . >"$DRV/two.out" 2>&1 || true
check "driver-reports-several-syntax-errors" "2" \
    "$(grep -c 'error: unexpected `;`' "$DRV/two.out")"

# A missing END is reported where the module gives out, with what was expected.
printf 'MODULE NoEnd;\n  VAR x: INTEGER;\nBEGIN\n  x := 1;\nIF x = 1 THEN x := 2\nEND NoEnd.\n' > "$DRV/NoEnd.mod"
"$OBC" check "$DRV/NoEnd.mod" -I . >"$DRV/noend.out" 2>&1 || true
check "driver-locates-a-module-level-error" "1" \
    "$(grep -c 'error: unexpected `NoEnd`' "$DRV/noend.out")"
check "driver-says-what-would-have-fitted" "1" \
    "$(grep -c '= note: expected END or ;' "$DRV/noend.out")"

# 7. Regression module: one line of output per defect the review found, each
#    line's expectation computed from the report rather than from a run.
"$OBC" run Regress.mod -I . --command Regress.Run >"$DRV/regress.got" 2>"$DRV/regress.log"
check "regressions-all-hold" "$(cat regress.expected.txt)" "$(cat "$DRV/regress.got")"

# 8. A recursive record type must compile (the type collector used to recurse
#    forever), and an Oberon identifier that is a C keyword must survive.
cat > "$DRV/Recur.mod" <<'MODEOF'
MODULE Recur;
  TYPE P = POINTER TO R; R = RECORD next: P; key: INTEGER END;
  VAR head: P;
  PROCEDURE Push*(k: INTEGER);
    VAR n: P;
  BEGIN NEW(n); n.key := k; n.next := head; head := n END Push;
BEGIN END Recur.
MODEOF
cat > "$DRV/Keyword.mod" <<'MODEOF'
MODULE Keyword;
  VAR int, while, char: INTEGER;
  PROCEDURE Run*;
  BEGIN int := 1; while := 2; char := 3; ASSERT(int + while + char = 6) END Run;
BEGIN END Keyword.
MODEOF
if "$OBC" build "$DRV/Recur.mod" -I "$DRV" -o "$DRV/recur" 2>"$DRV/recur.log" &&
   "$OBC" run "$DRV/Keyword.mod" -I "$DRV" --command Keyword.Run 2>>"$DRV/recur.log"; then
    echo "PASS recursive-types-and-keywords-compile"
    pass=$((pass + 1))
else
    echo "FAIL recursive-types-and-keywords-compile:"
    sed -n '1,20p' "$DRV/recur.log"
    fail=$((fail + 1))
fi

# 9. A failed type guard must trap (and say where), not read a stranger's memory.
# A failing guard must trap. The *pointer* spelling is used here because a
# record-spelled failed guard reads past the object by construction, which the C
# compiler can see statically and refuses under -Werror (DESIGN §13 R15).
cat > "$DRV/Guard.mod" <<'MODEOF'
MODULE Guard;
  TYPE R = RECORD a: INTEGER END; S = RECORD (R) b: INTEGER END;
       P = POINTER TO R; Q = POINTER TO S;
  VAR p: P;
  PROCEDURE Run*;
  BEGIN
    NEW(p); p.a := 1;
    IF p(Q) # NIL THEN ASSERT(FALSE) END;
    ASSERT(FALSE)
  END Run;
BEGIN END Guard.
MODEOF
if "$OBC" build "$DRV/Guard.mod" -I "$DRV" -o "$DRV/guard" --command Guard.Run \
        2>"$DRV/guard.cc.log"; then
    echo "PASS guard-module-compiles"
    pass=$((pass + 1))
else
    echo "FAIL guard-module-compiles:"
    sed -n '1,10p' "$DRV/guard.cc.log"
    fail=$((fail + 1))
fi
sh -c '"$0" >/dev/null 2>"$1"' "$DRV/guard" "$DRV/guard.err" || true
check "failed-guard-traps" "1" "$(grep -c 'type guard failed at byte' "$DRV/guard.err")"

echo "----"
echo "SUMMARY e2e: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
