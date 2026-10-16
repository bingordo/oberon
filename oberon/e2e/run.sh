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
CFLAGS="-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror -Wno-overflow"

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
OBC=${OBC:-../target/debug/obc}


# Line directives are opt-in, source-located, and wired through the CLI.
cat > "$OUT/LineMap.mod" <<'EOF'
MODULE LineMap;
VAR value: INTEGER;
BEGIN
  value := 1
END LineMap.
EOF
"$OBC" emit "$OUT/LineMap.mod" --out "$OUT" --line-directives >/dev/null
if grep -q '^#line 4 "' "$OUT/LineMap.c" &&
    $CC $CFLAGS -I "$OUT" -c "$OUT/LineMap.c" -o "$OUT/LineMap.o" 2>"$OUT/linemap.cc.log"; then
    echo "PASS c-line-directive-cli-and-compiles"
    pass=$((pass + 1))
else
    echo "FAIL c-line-directive-cli-and-compiles"
    cat "$OUT/linemap.cc.log" 2>/dev/null || true
    fail=$((fail + 1))
fi

# 1. Interface files are part of the build: the compiler writes them and reads
#    them back for the importing module.
cp Texts.sym Oberon.sym Texts.h Texts.c Oberon.h Oberon.c "$OUT"/
"$OB" Out.mod --emit-c "$OUT" -I "$OUT" >/dev/null
check "compiler-writes-an-interface" "MODULE Out" "$(head -1 "$OUT/Out.sym")"
check "interface-records-its-imports" "IMPORT Texts, Oberon" "$(sed -n '3p' "$OUT/Out.sym")"
check "interface-records-its-version" "VERSION 1" "$(sed -n '2p' "$OUT/Out.sym")"
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

# 3. It runs and prints the output specified by the report.
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

# 6. The driver reads source, resolves imports, invokes the C compiler, and
#    produces a runnable program.
OBC=${OBC:-../target/debug/obc}
# BACKEND=clif runs every check through the Cranelift back end instead of the C one
# (PLAN.md W9.1/W9.2). The driver takes `--backend` after the subcommand and the
# module, so a one-line wrapper is enough to re-point all hundred-odd call sites
# below rather than edit each of them into a build of its own.
if [ -n "${BACKEND:-}" ] && [ "$BACKEND" != c ]; then
    real_obc=$(cd "$(dirname -- "$OBC")" && pwd)/$(basename -- "$OBC")
    cat > "$DRV/obc-backend" <<WRAPPER
#!/bin/sh
exec "$real_obc" "\$@" --backend=$BACKEND
WRAPPER
    chmod +x "$DRV/obc-backend"
    OBC="$DRV/obc-backend"
fi
BACKEND_MODE=${BACKEND:-c}
# The C back end's driver, whatever BACKEND says. A row that reads the emitted *C*
# `Checks.c`, `Named.c`, `SysChk.c`, or a `.h` file concerns the C back end.
# inherit the switch, or the suite stops at the first such row instead of reaching
# the rows a different back end exists to be judged by.
C_OBC=${C_OBC:-../target/debug/obc}

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

# A file with sixty broken statements prints twenty errors and says how many it
# suppressed. Setting the error limit to zero means no limit (PLAN.md W5.2).
{
    printf 'MODULE Flood;\n  VAR x: INTEGER;\nBEGIN\n'
    for _ in $(seq 60); do printf '  x := ;\n'; done
    printf 'END Flood.\n'
} > "$DRV/Flood.mod"
"$OBC" check "$DRV/Flood.mod" --out "$DRV" >"$DRV/flood.out" 2>&1 || true
check "the-error-limit-caps-a-flood" "20" \
    "$(grep -c 'error:' "$DRV/flood.out")"
check "and-says-how-many-it-suppressed" "1" \
    "$(grep -c '40 more error(s) were suppressed' "$DRV/flood.out")"
"$OBC" check "$DRV/Flood.mod" --out "$DRV" --ferror-limit 0 >"$DRV/flood0.out" 2>&1 || true
check "zero-means-no-limit" "60" \
    "$(grep -c 'error:' "$DRV/flood0.out")"

# A NIL pointer dereference traps with the designator's offset, in both
# spellings (report section 6.4 treats NIL as an error; the design traps rather
# than faulting: R21, computed as a fault before this check existed).
for form in field caret; do
    if [ "$form" = field ]; then
        stmt='p.x := 1'
        body='TYPE R = RECORD x: INTEGER END; VAR p: POINTER TO R'
    else
        stmt='p^ := p^'
        body='TYPE R = RECORD x: INTEGER END; VAR p: POINTER TO R'
    fi
    {
        printf 'MODULE Nil%s;\n  %s;\nBEGIN\n  p := NIL;\n  %s\nEND Nil%s.\n' \
            "$form" "$body" "$stmt" "$form"
    } > "$DRV/Nil$form.mod"
    "$OBC" build "$DRV/Nil$form.mod" --out "$DRV" -o "$DRV/nil$form" >/dev/null 2>&1 || true
    sh -c '"$0" >/dev/null 2>"$1"' "$DRV/nil$form" "$DRV/nil$form.err" || true
    check "nil-$form-dereference-traps" "1" \
        "$(grep -c 'NIL pointer dereference (at byte' "$DRV/nil$form.err")"
done

# An unreadable interface is explained only if the build needs it. An unrelated
# file on the search path must not be reported (PLAN.md W3.2).
mkdir -p "$DRV/badiface"
printf 'MODULE Unrelated\nTYPE T = INTEGER\n' > "$DRV/badiface/Unrelated.sym"
printf 'MODULE Quiet;\nBEGIN\nEND Quiet.\n' > "$DRV/Quiet.mod"
"$OBC" check "$DRV/Quiet.mod" -I "$DRV/badiface" --out "$DRV" >"$DRV/quiet.out" 2>&1 || true
check "an-unrelated-refused-interface-is-not-reported" "0" \
    "$(grep -c 'Unrelated.sym' "$DRV/quiet.out")"
printf 'MODULE Needs;\n  IMPORT Unrelated;\nBEGIN\nEND Needs.\n' > "$DRV/Needs.mod"
"$OBC" check "$DRV/Needs.mod" -I "$DRV/badiface" --out "$DRV" >"$DRV/needs.out" 2>&1 || true
check "a-refused-interface-that-is-needed-is-explained" "1" \
    "$(grep -c 'Unrelated.sym: no VERSION line' "$DRV/needs.out")"

# The trap matrix: one deliberate violation per kind, each in its own module (the
# first trap aborts the process), each asserting the message and the offset rather
# than merely that something went wrong (PLAN.md W7.4).
trap_case() { # name source expected-line
    printf '%s' "$2" > "$DRV/$1.mod"
    "$OBC" build "$DRV/$1.mod" --out "$DRV" -o "$DRV/$1" >/dev/null 2>&1 || {
        echo "FAIL trap-$1: did not build"
        fail=$((fail + 1))
        return
    }
    status=0
    sh -c '"$0" >/dev/null 2>"$1"' "$DRV/$1" "$DRV/$1.err" || status=$?
    # The expected line is exact, source offset included, so a row fails if the
    # check disappears, moves, or reports the wrong place; and a trap is an
    # abort, so the status is asserted rather than the message alone.
    check "trap-$1" "1" "$(grep -cF -- "$3" "$DRV/$1.err")"
    check "trap-$1-aborts" "134" "$status"
}

trap_case index-below-zero 'MODULE T1;
  VAR a: ARRAY 4 OF INTEGER; i: INTEGER;
BEGIN
  i := (-1);
  a[i] := 1
END T1.
' 'oberon: trap: index out of range (at byte 73 of the source)'
trap_case index-at-len 'MODULE T2;
  VAR a: ARRAY 4 OF INTEGER;
BEGIN
  a[4] := 1
END T2.
' 'oberon: trap: index out of range (at byte 48 of the source)'
trap_case index-in-open-array 'MODULE T3;
  VAR a: ARRAY 4 OF INTEGER;
  PROCEDURE Fill(VAR v: ARRAY OF INTEGER);
  BEGIN
    v[4] := 1
  END Fill;
BEGIN
  Fill(a)
END T3.
' 'oberon: trap: index out of range (at byte 95 of the source)'
# A failed guard traps. The *record* spelling is covered here through a record
# Copying the guarded view tests a record guard. Reading a derived *field*
# through the view is the static-analysis problem R15 records: gcc sees a
# read beyond the allocation and -Werror rejects the program, although the
# guard traps first. The *pointer* spelling is covered by the existing
# `failed-guard-traps` row.
trap_case guard-record-spelling 'MODULE T4;
  TYPE R = RECORD END; S = RECORD (R) f: INTEGER END; P = POINTER TO R;
  VAR p: P; s: S;
BEGIN
  NEW(p);
  s := p(S)
END T4.
' 'oberon: trap: type guard failed at byte 124'
# The *record* spelling of a failing guard is tested as an assignment target.
# The write must compile under the documented line and trap with the guard's
# offset. gcc used to reject it because it folded an inlined
# `malloc(sizeof(R))` and proved the write lay outside the allocation
# (DESIGN R15, PLAN.md W2.2).
guard_record=$(cat <<'GuardREOF'
MODULE GuardR;
  TYPE R = RECORD f: INTEGER END; S = RECORD (R) g: INTEGER END;
       P = POINTER TO R; PS = POINTER TO S;
  VAR p: P;
BEGIN
  NEW(p);          (* p points at an R *)
  p(S).g := 1      (* the guard fails, and the write must not be rejected *)
END GuardR.
GuardREOF
)
trap_case guard-record-spelling-as-target "$guard_record" 'oberon: trap: type guard failed at byte 186'
# The same program is also tested under clang, as promised by the design row.
if command -v clang >/dev/null 2>&1; then
    printf '%s' "$guard_record" > "$DRV/GuardR.mod"
    if "$OBC" build "$DRV/GuardR.mod" --cc clang --out "$DRV" -o "$DRV/guardr" 2>"$DRV/guardr.log"; then
        status=0
        sh -c '"$0" >/dev/null 2>"$1"' "$DRV/guardr" "$DRV/guardr.err" || status=$?
        check "guard-record-spelling-clang-traps" "1" \
            "$(grep -c 'type guard failed at byte' "$DRV/guardr.err")"
        check "guard-record-spelling-clang-aborts" "134" "$status"
    else
        echo "FAIL guard-record-spelling-clang: clang rejected the emitted C"
        sed -n '1,6p' "$DRV/guardr.log"
        fail=$((fail + 1))
    fi
else
    echo "SKIP guard-record-spelling-clang: no clang on this machine"
fi
# `-Ochecks=n` compiles the runtime checks out (PLAN.md W3.1): the same module
# traps by default and reads on without them, and the emitted C of the unchecked
# build carries no check at all.
cat > "$DRV/Checks.mod" <<'MODEOF'
MODULE Checks;
  VAR a: ARRAY 100 OF INTEGER; n: INTEGER;
BEGIN
  n := a[150]
END Checks.
MODEOF
if "$C_OBC" build "$DRV/Checks.mod" --out "$DRV" -o "$DRV/checks-on" >/dev/null 2>&1 &&
   "$C_OBC" build "$DRV/Checks.mod" -Ochecks=n --out "$DRV" -o "$DRV/checks-off" >/dev/null 2>&1; then
    status=0
    sh -c '"$0" >/dev/null 2>"$1"' "$DRV/checks-on" "$DRV/checks-on.err" || status=$?
    check "checks-on-traps" "134" "$status"
    check "checks-on-names-the-offset" "1" \
        "$(grep -c 'index out of range (at byte' "$DRV/checks-on.err")"
    status=0
    "$DRV/checks-off" || status=$?
    check "checks-off-runs" "0" "$status"
    check "checks-off-emits-no-check" "0" \
        "$(grep -c 'rt_index\|rt_nil\|rt_guard_is' "$DRV/Checks.c")"
# Every check has a name, and selecting a check emits only that check (W8.3).
    printf 'MODULE Named;\n  TYPE R = RECORD x: INTEGER END; P = POINTER TO R;\n  VAR a: ARRAY 100 OF INTEGER; p: P; n: INTEGER;\nBEGIN\n  n := a[150]; p.x := 1\nEND Named.\n' > "$DRV/Named.mod"
    "$C_OBC" emit "$DRV/Named.mod" -Ochecks=nil --out "$DRV" >/dev/null 2>&1
    check "named-checks-emit-only-that-check" "1" \
        "$(grep -c 'rt_nil' "$DRV/Named.c")"
    check "named-checks-leave-the-others-out" "0" \
        "$(grep -c 'rt_index' "$DRV/Named.c")"
    "$C_OBC" emit "$DRV/Named.mod" -Ochecks=index --out "$DRV" >/dev/null 2>&1
    check "named-checks-can-turn-one-back-on" "1" \
        "$(grep -c 'rt_index' "$DRV/Named.c")"
    # And an unknown name is a diagnostic rather than a silent default.
    if "$C_OBC" emit "$DRV/Named.mod" -Ochecks=banana --out "$DRV" 2>"$DRV/banana.log"; then
        echo "FAIL unknown-check-name-is-refused: the flag was accepted"
        fail=$((fail + 1))
    else
        check "unknown-check-name-is-refused" "1" "$(grep -c 'names no check' "$DRV/banana.log")"
    fi
else
    echo "FAIL checks-flag: the module did not build either way"
    fail=$((fail + 1))
fi

# A scalar CASE with no default arm warns and traps with the subject's offset
# if no label matches (W2.5). Type CASE has always worked that way.
trap_case case-no-matching-label 'MODULE Cnl;
  VAR i: INTEGER; ok: ARRAY 1 OF INTEGER;
BEGIN
  i := 5;
  CASE i OF 1: i := 1 | 2: i := 2 END;
  ok[1] := 0
END Cnl.
' 'oberon: trap: CASE: no matching label (at byte'
printf 'MODULE CnlW;\n  VAR i: INTEGER;\nBEGIN\n  i := 1;\n  CASE i OF 1: i := 2 END\nEND CnlW.\n' > "$DRV/CnlW.mod"
"$OBC" check "$DRV/CnlW.mod" >"$DRV/cnlw.out" 2>&1 || true
check "a-default-less-case-warns" "1" "$(grep -c 'this CASE has no default arm' "$DRV/cnlw.out")"

trap_case case-no-match 'MODULE T6;
  TYPE R = RECORD END; S = RECORD (R) f: INTEGER END; P = POINTER TO R; PS = POINTER TO S;
  VAR p: P;
BEGIN
  NEW(p);
  CASE p OF PS: END
END T6.
' 'oberon: trap: CASE: no matching type (at byte 137 of the source)'
trap_case div-by-zero 'MODULE T7;
  VAR i, j: INTEGER;
BEGIN
  j := 0;
  i := 7 DIV j
END T7.
' 'oberon: trap: DIV by zero (at byte 55 of the source)'
trap_case mod-by-zero 'MODULE T8;
  VAR i, j: INTEGER;
BEGIN
  j := 0;
  i := 7 MOD j
END T8.
' 'oberon: trap: MOD by zero (at byte 55 of the source)'
trap_case failing-assert 'MODULE T9;
BEGIN
  ASSERT(1 = 2)
END T9.
' 'oberon: trap: assertion failed (at byte 19 of the source)'

# `NEW` is specified to yield NIL on failure (report section 6.4). The hook makes
# this observable: the program traps itself if the allocation did not fail.
printf 'MODULE T10;
  TYPE R = RECORD x: INTEGER END;
  VAR p: POINTER TO R; fail: ARRAY 1 OF INTEGER;
BEGIN
  NEW(p);
  IF p # NIL THEN fail[1] := 0 END
END T10.
' > "$DRV/T10.mod"
"$OBC" build "$DRV/T10.mod" --out "$DRV" -o "$DRV/T10" >/dev/null 2>&1 || true
status=0
OB_ALLOC_FAIL_AT=1 sh -c '"$0" >/dev/null 2>&1' "$DRV/T10" || status=$?
check "a-failing-new-yields-nil" "0" "$status"

# W10.2's collector: a program whose live set stays bounded while its allocation
# count grows must run in a bounded heap. `OB_GC_LIMIT` is a hard bound, so with
# nothing reclaiming, `NEW` would give out after 85 of the 20000 allocations
# (a 4096-byte heap, a 48-byte block), the NIL dereference would trap, and the
# program would die; with the collector it completes, and the sum it checks says
# No node of a live chain was reclaimed. Test collection on every allocation,
# collection triggered by the limit, and idle collection (`OB_GC_STRESS` and
# `OB_GC_LIMIT` unset). The idle run shows that programs that do not need the
# collector pay nothing for it.
cat > "$DRV/Gc.mod" <<'MODEOF'
MODULE Gc;
  TYPE P = POINTER TO Node;
       Node = RECORD x: INTEGER; next: P END;
  VAR total, i: INTEGER; fail: ARRAY 2 OF INTEGER;

  PROCEDURE Round(n: INTEGER): INTEGER;
    VAR head, p: P; s: INTEGER;
  BEGIN
    head := NIL; s := 0;
    WHILE n > 0 DO NEW(p); p.x := n; p.next := head; head := p; n := n - 1 END;
    WHILE head # NIL DO s := s + head.x; head := head.next END;
    RETURN s
  END Round;
BEGIN
  total := 0;
  FOR i := 1 TO 1000 DO total := total + Round(20) END;
  IF total # 210000 THEN fail[2] := 0 END
END Gc.
MODEOF
if "$OBC" build "$DRV/Gc.mod" --out "$DRV" -o "$DRV/gc" >/dev/null 2>&1; then
    # 1000 rounds of 20 nodes make 20000 allocations. Each round's chain is dead
# the next one starts. The live set is one 20-node chain (960 bytes: a
# 24-byte header and a 24-byte node per allocation). The whole run would need
# 960000 bytes. 210000 is the sum of values 1..20, repeated a thousand times.
    for mode in stress limit idle; do
        status=0
        case $mode in
            stress) OB_GC_STRESS=1 OB_GC_LIMIT=4096 "$DRV/gc" || status=$? ;;
            limit) OB_GC_LIMIT=4096 "$DRV/gc" || status=$? ;;
            idle) "$DRV/gc" || status=$? ;;
        esac
        check "gc-runs-in-a-bounded-heap-$mode" "0" "$status"
    done
else
    echo "FAIL gc-runs-in-a-bounded-heap: the module did not build"
    fail=$((fail + 1))
fi

# A live set larger than the limit: `NEW` yields NIL (report section 6.4), and
# allocation stops. Collection that frees nothing must not loop. Every node
# that fit remains, as the count confirms.
cat > "$DRV/GcNil.mod" <<'MODEOF'
MODULE GcNil;
  TYPE P = POINTER TO Node;
       Node = RECORD x: INTEGER; next: P END;
  VAR head, p: P; k, count: INTEGER; done: BOOLEAN; fail: ARRAY 2 OF INTEGER;
BEGIN
  head := NIL; k := 0; done := FALSE;
  WHILE (~done) & (k < 100) DO
    NEW(p);
    IF p = NIL THEN done := TRUE ELSE p.x := k; p.next := head; head := p; k := k + 1 END
  END;
  IF ~done THEN fail[2] := 0 END;                 (* the limit must have bitten *)
  IF (k < 2) OR (k = 100) THEN fail[2] := 0 END;  (* and only after two fitted *)
  count := 0; p := head;
  WHILE p # NIL DO count := count + 1; p := p.next END;
  IF count # k THEN fail[2] := 0 END              (* and nothing live was reclaimed *)
END GcNil.
MODEOF
if "$OBC" build "$DRV/GcNil.mod" --out "$DRV" -o "$DRV/gcnil" >/dev/null 2>&1; then
    status=0
    OB_GC_LIMIT=128 "$DRV/gcnil" || status=$?
    check "a-live-set-over-the-limit-yields-nil" "0" "$status"
else
    echo "FAIL a-live-set-over-the-limit-yields-nil: the module did not build"
    fail=$((fail + 1))
fi

# The roots the collector has to find (W10.2), in the two shapes `Gc.mod` does
# not have: an ARRAY OF POINTER at module level, and a record type whose map is
# emitted in *another* module's translation unit. `GcDef` is a module, not a
# hand-written fixture, so the driver checks and compiles it like any other.
cat > "$DRV/GcDef.mod" <<'MODEOF'
MODULE GcDef;
  TYPE Node* = RECORD val*: INTEGER END;
       P* = POINTER TO Node;
END GcDef.
MODEOF
cat > "$DRV/GcRoots.mod" <<'MODEOF'
MODULE GcRoots;
  IMPORT GcDef;
  VAR runs: ARRAY 3 OF GcDef.P; i, sum: INTEGER; fail: ARRAY 2 OF INTEGER;

  PROCEDURE Fill(VAR v: ARRAY OF GcDef.P; n: INTEGER);
    VAR k: INTEGER; p: GcDef.P;
  BEGIN
    k := 0;
    WHILE k < n DO
      NEW(p);
      IF p # NIL THEN p.val := k + 1; v[k] := p END;
      k := k + 1
    END
  END Fill;

  PROCEDURE Churn(n: INTEGER);
    VAR p: GcDef.P;
  BEGIN
    WHILE n > 0 DO NEW(p); p.val := 0; p := NIL; n := n - 1 END
  END Churn;
BEGIN
  Fill(runs, 3);
  Churn(100);
  sum := 0; i := 0;
  WHILE i < 3 DO sum := sum + runs[i].val; i := i + 1 END;
  FOR i := 1 TO 20 DO Churn(5) END;
  IF sum # 6 THEN fail[2] := 0 END   (* 200 collections later, all three are still 1, 2, 3 *)
END GcRoots.
MODEOF
if "$OBC" build "$DRV/GcRoots.mod" --out "$DRV" -I "$DRV" -o "$DRV/gcroots" >/dev/null 2>&1; then
    status=0
    OB_GC_STRESS=1 "$DRV/gcroots" || status=$?
    check "gc-roots-hold-their-objects" "0" "$status"
else
    echo "FAIL gc-roots-hold-their-objects: the modules did not build"
    fail=$((fail + 1))
fi

# A pointer field declared in the object's base type: the object's own
# descriptor is the extension's, so the map must reach the field one level in
# through `offsetof(S, base) + offsetof(R, q)`. `head.q` is the only reference
# to the leaf object, so a map that misses the inherited field loses it.
cat > "$DRV/GcExt.mod" <<'MODEOF'
MODULE GcExt;
  TYPE Q = POINTER TO Leaf;
       Leaf = RECORD y: INTEGER END;
       R = RECORD a: INTEGER; q: Q; pad: ARRAY 2 OF Leaf END;
       S = RECORD (R) b: INTEGER END;
  VAR head: POINTER TO S; leaf: Q; total: INTEGER; fail: ARRAY 2 OF INTEGER;

  PROCEDURE Churn(n: INTEGER);
    VAR p: Q;
  BEGIN
    WHILE n > 0 DO NEW(p); p.y := 0; p := NIL; n := n - 1 END
  END Churn;
BEGIN
  NEW(leaf); leaf.y := 7;
  NEW(head); head.a := 0; head.b := 0; head.q := leaf;
  head.pad[1] := leaf^;                    (* a record inside the base as well *)
  Churn(200);
  total := head.q.y;
  IF total # 7 THEN fail[2] := 0 END
END GcExt.
MODEOF
if "$OBC" build "$DRV/GcExt.mod" --out "$DRV" -o "$DRV/gcext" >/dev/null 2>&1; then
    status=0
    OB_GC_STRESS=1 "$DRV/gcext" || status=$?
    check "gc-an-inherited-pointer-field-is-a-root" "0" "$status"
else
    echo "FAIL gc-an-inherited-pointer-field-is-a-root: the module did not build"
    fail=$((fail + 1))
fi

# Both syntax errors in one body are reported.
printf 'MODULE Two;\n  VAR x, y: INTEGER;\nBEGIN\n  x := ;\n  y := ;\n  x := 1\nEND Two.\n' > "$DRV/Two.mod"
"$OBC" check "$DRV/Two.mod" -I . >"$DRV/two.out" 2>&1 || true
check "driver-reports-several-syntax-errors" "2" \
    "$(grep -c 'error: unexpected `;`' "$DRV/two.out")"

# A missing END is reported at the end of the module, with the expected token.
printf 'MODULE NoEnd;\n  VAR x: INTEGER;\nBEGIN\n  x := 1;\nIF x = 1 THEN x := 2\nEND NoEnd.\n' > "$DRV/NoEnd.mod"
"$OBC" check "$DRV/NoEnd.mod" -I . >"$DRV/noend.out" 2>&1 || true
check "driver-locates-a-module-level-error" "1" \
    "$(grep -c 'error: unexpected `NoEnd`' "$DRV/noend.out")"
# `;` leads the expected list when it would have fitted: a missing separator is
# the commonest defect, so the note names it first.
check "driver-says-what-would-have-fitted" "1" \
    "$(grep -c '= note: expected ; .*END' "$DRV/noend.out")"

# A module that never reads its own variable, local or procedure must still
# compile: C rejects an unmentioned `static` and an unread parameter under the
# documented build line, and Oberon requires none of them (R20, PLAN.md W6.2).
cat > "$DRV/Unused.mod" <<'MODEOF'
MODULE Unused;
  VAR neverRead, neverSet: INTEGER;
  PROCEDURE neverCalled(n: INTEGER): INTEGER;
    VAR neverUsed: INTEGER;
  BEGIN neverUsed := n; RETURN n END neverCalled;
BEGIN neverRead := 0
END Unused.
MODEOF
if "$OBC" build "$DRV/Unused.mod" -I "$DRV" -o "$DRV/unused" 2>"$DRV/unused.log"; then
    echo "PASS unused-entities-still-compile"
    pass=$((pass + 1))
else
    echo "FAIL unused-entities-still-compile:"
    sed -n '1,6p' "$DRV/unused.log"
    fail=$((fail + 1))
fi

# 7. Regression module: one line of output per defect the review found, each
#    line's expectation computed from the report rather than from a run.
# A back end that cannot lower this module yet reports a FAIL instead of
# aborting. The later rows still run, which measures the Cranelift back end's
# progress.
if "$OBC" run Regress.mod -I . --command Regress.Run >"$DRV/regress.got" 2>"$DRV/regress.log"; then
    check "regressions-all-hold" "$(cat regress.expected.txt)" "$(cat "$DRV/regress.got")"
else
    echo "FAIL regressions-all-hold:"
    sed -n '1,6p' "$DRV/regress.log"
    fail=$((fail + 1))
fi

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
# compiler can see statically and refuses under -Werror (DESIGN section 13 R15).
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

# A designator narrowed by a type-CASE arm or guard stays *assignable*. The cast
# that gives a derived field its layout is not itself an lvalue. Use the plain
# spelling when storing through a narrowed name that is the whole designator
# (DESIGN R29, PLAN.md W7.6).
runs_case() { # name source
    printf '%s' "$2" > "$DRV/$1.mod"
    "$OBC" build "$DRV/$1.mod" --out "$DRV" -o "$DRV/$1" >/dev/null 2>&1 || {
        echo "FAIL $1: did not build"
        fail=$((fail + 1))
        return
    }
    status=0
    "$DRV/$1" || status=$?
    check "$1" "0" "$status"
}
cat > "$DRV/ConstDefs.mod" <<'EOF'
MODULE ConstDefs;
CONST Code* = 0AX; Text* = "abc";
END ConstDefs.
EOF
runs_case imported-char-string-constants 'MODULE ConstUse;
  IMPORT ConstDefs;
  VAR code: CHAR; text: ARRAY 4 OF CHAR;
  PROCEDURE CheckText(a: ARRAY OF CHAR);
  BEGIN ASSERT(LEN(a) = 3) END CheckText;
BEGIN
  code := ConstDefs.Code;
  text := ConstDefs.Text;
  ASSERT(code = 0AX);
  ASSERT(text = "abc");
  CheckText(ConstDefs.Text);
  ASSERT(ConstDefs.Text = "abc")
END ConstUse.
'
runs_case narrow-assign-to-narrowed 'MODULE N1;
  TYPE R = RECORD END; S = RECORD (R) f: INTEGER END; P = POINTER TO R; PS = POINTER TO S;
  VAR p: P; ps: PS; ok: ARRAY 1 OF INTEGER;
BEGIN
  NEW(ps); p := ps;
  CASE p OF PS: p := NIL | : ok[1] := 0 END;
  IF p # NIL THEN ok[1] := 0 END
END N1.
'
runs_case narrow-field-through-narrowed 'MODULE N2;
  TYPE R = RECORD END; S = RECORD (R) f: INTEGER END; P = POINTER TO R; PS = POINTER TO S;
  VAR p: P; ps: PS; ok: ARRAY 1 OF INTEGER;
BEGIN
  NEW(ps); p := ps;
  CASE p OF PS: p.f := 7 | : ok[1] := 0 END;
  CASE p OF PS: IF p.f # 7 THEN ok[1] := 0 END | : ok[1] := 0 END
END N2.
'
runs_case narrow-guard-as-target 'MODULE N3;
  TYPE R = RECORD f: INTEGER END; S = RECORD (R) g: INTEGER END;
       P = POINTER TO R; PS = POINTER TO S;
  VAR p: P; ps: PS; s: S; ok: ARRAY 1 OF INTEGER;
  PROCEDURE take(q: PS); BEGIN IF q.g # 7 THEN ok[1] := 0 END END take;
BEGIN
  NEW(ps); ps.f := 1; ps.g := 7; p := ps;
  p(PS).g := 7;
  s := p(S); IF s.g # 7 THEN ok[1] := 0 END;
  take(p(PS))
END N3.
'
# A record value argument inside a short-circuit operand is copied where the
# operand is evaluated, not one step earlier: `idx()` must not run when `&`
# short-circuits, and must run when it does not (R16, PLAN.md W2.3).
runs_case copy-inside-shortcircuit 'MODULE R16;
  TYPE Rec = RECORD a: INTEGER END;
  VAR arr: ARRAY 2 OF Rec; n: INTEGER; ok: ARRAY 1 OF INTEGER;
  PROCEDURE idx(): INTEGER; BEGIN n := n + 1; RETURN 0 END idx;
  PROCEDURE take(x: Rec): INTEGER; BEGIN RETURN x.a END take;
BEGIN
  arr[0].a := 5; n := 0;
  IF FALSE & (take(arr[idx()]) = 5) THEN ok[1] := 0 END;
  IF n # 0 THEN ok[1] := 0 END;
  n := 0;
  IF TRUE & (take(arr[idx()]) = 5) THEN ELSE ok[1] := 0 END;
  IF n # 1 THEN ok[1] := 0 END
END R16.
'

# 10. report section 11.1's SYSTEM: one program asserts all six entries in
# the table (Sys.mod), tests the import that resolves to a builtin, checks the
# implementation-specific entries are refused by name, and confirms that
# `-Ochecks` does not apply here.
if "$OBC" build Sys.mod -I . --out "$DRV" -o "$DRV/sys" 2>"$DRV/sys.log"; then
    echo "PASS system-entries-compile"
    pass=$((pass + 1))
else
    echo "FAIL system-entries-compile:"
    sed -n '1,20p' "$DRV/sys.log"
    fail=$((fail + 1))
fi
status=0
"$DRV/sys" || status=$?
check "system-entries-run[ADR, SIZE, BIT, GET, PUT, COPY]" "0" "$status"
# The import is in the interface, so a client can see that this module is not
# portable (PLAN.md W4.1 step 4). SYSTEM has no `.h` to include.
check "system-import-is-recorded-in-the-interface" "IMPORT SYSTEM" "$(sed -n 3p "$DRV/Sys.sym")"
mkdir -p "$DRV/sysc"
"$C_OBC" emit Sys.mod -I . --out "$DRV/sysc" >/dev/null 2>&1 || true
check "system-has-no-header-to-include" "0" \
    "$(grep -c 'SYSTEM\.h' "$DRV/sysc/Sys.h" || true)"

# The compiler owns the SYSTEM module. It never reads, compiles, or links a
# `SYSTEM.mod`/`SYSTEM.sym` on the search path, so a broken pair cannot affect a
# build. A user-supplied module would be a different language, and programs that
# import SYSTEM must get the report's entries.
mkdir -p "$DRV/shadow"
printf 'MODULE SYSTEM; VAR this: INTEGER; BEGIN this := ;\n' > "$DRV/shadow/SYSTEM.mod"
printf 'MODULE SYSTEM\nTYPE Garbage = RECORD x: INTEGER\n' > "$DRV/shadow/SYSTEM.sym"
if "$OBC" build Sys.mod -I . -I "$DRV/shadow" --out "$DRV/shadowout" -o "$DRV/shadow-sys" \
        >"$DRV/shadow.log" 2>&1; then
    echo "PASS system-on-the-search-path-is-shadowed"
    pass=$((pass + 1))
else
    echo "FAIL system-on-the-search-path-is-shadowed:"
    sed -n '1,10p' "$DRV/shadow.log"
    fail=$((fail + 1))
fi
check "shadowed-system-is-not-compiled" "0" "$(ls "$DRV/shadowout"/SYSTEM.* 2>/dev/null | wc -l)"
status=0
"$DRV/shadow-sys" || status=$?
check "shadowed-build-still-runs" "0" "$status"

# SYSTEM is the escape hatch, so `-Ochecks=n` must not add *or remove* anything:
# a module whose memory accesses are all SYSTEM's emits byte-identical C either
# way, and carries no runtime check to begin with.
cat > "$DRV/SysChk.mod" <<'MODEOF'
MODULE SysChk;
  IMPORT SYSTEM;
  TYPE R = RECORD a: INTEGER END;
  VAR v, got: INTEGER; ch: CHAR; r: REAL; f: BOOLEAN; src, dst: ARRAY 4 OF INTEGER;
BEGIN
  SYSTEM.PUT(SYSTEM.ADR(v), 1);
  SYSTEM.GET(SYSTEM.ADR(v), got);
  f := SYSTEM.BIT(SYSTEM.ADR(got), 0);
  v := SYSTEM.ADR(v) + SYSTEM.SIZE(R) + SYSTEM.SIZE(INTEGER) + SYSTEM.SIZE(CHAR);
  SYSTEM.COPY(SYSTEM.ADR(src), SYSTEM.ADR(dst), 4);
  SYSTEM.PUT(SYSTEM.ADR(ch), ch);
  SYSTEM.GET(SYSTEM.ADR(ch), ch);
  SYSTEM.PUT(SYSTEM.ADR(r), r)
END SysChk.
MODEOF
if "$C_OBC" emit "$DRV/SysChk.mod" --out "$DRV/chk-on" >/dev/null 2>&1 &&
   "$C_OBC" emit "$DRV/SysChk.mod" --out "$DRV/chk-off" -Ochecks=n >/dev/null 2>&1; then
    same=0
    cmp -s "$DRV/chk-on/SysChk.c" "$DRV/chk-off/SysChk.c" || same=1
    check "system-emits-the-same-c-under--Ochecks=n" "0" "$same"
    check "system-emits-no-runtime-check" "0" \
        "$(grep -c 'rt_index\|rt_nil\|rt_guard_is' "$DRV/chk-on/SysChk.c")"
else
    echo "FAIL system-checks-flag: the module did not emit either way"
    fail=$((fail + 1))
fi

# Check that each refusal is located and that the extras name the rejected
# operation (`SYSTEM.ADC is not implemented`, not a silent no-op).
cat > "$DRV/SysBad.mod" <<'MODEOF'
MODULE SysBad;
  IMPORT SYSTEM;
  CONST K = 7;
  TYPE R = RECORD a: INTEGER END;
  VAR i: INTEGER; r: R; a: ARRAY 4 OF INTEGER;
BEGIN
  SYSTEM.ADC(i, i);
  SYSTEM.VAL(INTEGER, i);
  SYSTEM.LED(3);
  SYSTEM.GET(i, r);
  SYSTEM.PUT(i, a);
  i := SYSTEM.SIZE(i);
  i := SYSTEM.ADR(1);
  SYSTEM.GET(i, K);
  SYSTEM.COPY(i, i)
END SysBad.
MODEOF
"$OBC" check "$DRV/SysBad.mod" -I . >"$DRV/sysbad.out" 2>&1 || true
for refused in ADC VAL LED; do
    check "system-$refused-is-refused-by-name" "1" \
        "$(grep -c "SYSTEM.$refused is not implemented" "$DRV/sysbad.out")"
done
check "system-get-needs-a-basic-variable" "1" \
    "$(grep -c 'argument 2 of SYSTEM.GET must be of a basic type' "$DRV/sysbad.out")"
check "system-put-needs-a-basic-value" "1" \
    "$(grep -c 'argument 2 of SYSTEM.PUT must be of a basic type' "$DRV/sysbad.out")"
check "system-size-needs-a-type-name" "1" \
    "$(grep -c 'the argument of SYSTEM.SIZE must be a type name' "$DRV/sysbad.out")"
check "system-adr-needs-an-address" "1" \
    "$(grep -c 'the argument of SYSTEM.ADR must be a variable' "$DRV/sysbad.out")"
check "system-get-target-must-be-writable" "1" \
    "$(grep -c '`K` is a constant, which is read-only' "$DRV/sysbad.out")"
check "system-arity-is-checked" "1" \
    "$(grep -c 'SYSTEM.COPY takes 3 argument(s), but 2 were given' "$DRV/sysbad.out")"
# `SYSTEM` is a module, so it has to be imported like one (row 3's second shape).
printf 'MODULE SysNoImp;\n  VAR i: INTEGER;\nBEGIN\n  i := SYSTEM.ADR(i)\nEND SysNoImp.\n' \
    > "$DRV/SysNoImp.mod"
"$OBC" check "$DRV/SysNoImp.mod" -I . >"$DRV/sysnoimp.out" 2>&1 || true
check "system-must-be-imported" "1" \
    "$(grep -c '`SYSTEM` is not declared' "$DRV/sysnoimp.out")"

# `MIN(T)`/`MAX(T)` (W8.2) and `CASE ... ELSE` (W8.1): the extremes are
# constants wherever they appear, and ELSE is report section 9.5's default
# arm under a keyword LALR(1) can parse.
runs_case min-max-and-case-else 'MODULE Ext;
  CONST Low = MIN(INTEGER); High = MAX(INTEGER);
  VAR i: INTEGER; ok: ARRAY 1 OF INTEGER;
BEGIN
  IF Low # (0 - 2147483647 - 1) THEN ok[1] := 0 END;
  IF High # 2147483647 THEN ok[1] := 0 END;
  IF MAX(BYTE) # 255 THEN ok[1] := 0 END;
  IF MIN(CHAR) # 0 THEN ok[1] := 0 END;
  i := MIN(INTEGER);
  IF i >= 0 THEN ok[1] := 0 END;
  CASE i OF 1: i := 1 | 2: i := 2 ELSE i := 7 END;
  IF i # 7 THEN ok[1] := 0 END;
  CASE 5 OF 1: i := 1 ELSE i := 9 END;
  IF i # 9 THEN ok[1] := 0 END
END Ext.
'
# The compiler reports an error for a second default arm.
printf 'MODULE Two;\n  VAR i: INTEGER;\nBEGIN\n  CASE i OF 1: i := 1 | ELSE i := 2 END\nEND Two.\n' > "$DRV/Two.mod"
"$OBC" check "$DRV/Two.mod" >"$DRV/two.out" 2>&1 || true
check "two-case-defaults-are-refused" "1" "$(grep -c 'two default arms' "$DRV/two.out")"

# `EXIT` (W8.5) leaves the innermost loop of every form, and `RETURN` (W8.4) is a
# statement: early, from inside a `CASE`, and naked in a proper procedure.
runs_case exit-and-early-return 'MODULE Ctl;
  VAR i, k: INTEGER; ok: ARRAY 1 OF INTEGER;
  PROCEDURE f(x: INTEGER): INTEGER;
  BEGIN
    IF x < 0 THEN RETURN 0 END;
    (* the default arm does what falling out of the CASE used to do, now that a
       value matching no label traps (W2.5) *)
    CASE x OF 1: RETURN 10 | 2: RETURN 20 ELSE RETURN 7 END
  END f;
  PROCEDURE stop(n: INTEGER);
  BEGIN
    IF n > 3 THEN RETURN END;
    IF n < 0 THEN ok[1] := 0 END
  END stop;
BEGIN
  k := 0;
  FOR i := 0 TO 9 DO IF i = 3 THEN EXIT END; k := k + 1 END;
  IF k # 3 THEN ok[1] := 0 END;
  WHILE TRUE DO EXIT END;
  REPEAT k := k + 1; EXIT UNTIL TRUE;
  IF k # 4 THEN ok[1] := 0 END;
  IF f(-5) # 0 THEN ok[1] := 0 END;
  IF f(1) # 10 THEN ok[1] := 0 END;
  IF f(5) # 7 THEN ok[1] := 0 END;
  stop(4); stop(1)
END Ctl.
'
# `EXIT` outside a loop has no loop to leave.
printf 'MODULE Lost;\nBEGIN\n  EXIT\nEND Lost.\n' > "$DRV/Lost.mod"
"$OBC" check "$DRV/Lost.mod" >"$DRV/lost.out" 2>&1 || true
check "exit-outside-a-loop-is-refused" "1" "$(grep -c 'only allowed inside a loop' "$DRV/lost.out")"

# A pointer array of a record declared later in the same module, and the same pair
# across a module boundary: both used to fail, the first on the emitted C and the
# second by naming the pointee differently in the header and the interface (R32,
# PLAN.md W2.4).
runs_case pointer-array-of-a-later-record 'MODULE Later;
  TYPE P = POINTER TO R; R = RECORD x: INTEGER END;
  VAR a: ARRAY 2 OF P; ok: ARRAY 1 OF INTEGER;
BEGIN
  NEW(a[0]); a[0].x := 1; NEW(a[1]); a[1].x := 2;
  IF a[0].x + a[1].x # 3 THEN ok[1] := 0 END
END Later.
'
cat > "$DRV/Def.mod" <<'MODEOF'
MODULE Def;
  TYPE P* = POINTER TO Node; Node* = RECORD k*: INTEGER END;
  PROCEDURE Get*(VAR p: P);
  BEGIN NEW(p); p.k := 5 END Get;
END Def.
MODEOF
cat > "$DRV/UseDef.mod" <<'MODEOF'
MODULE UseDef;
  IMPORT Def;
  VAR p: Def.P; ok: ARRAY 1 OF INTEGER;
BEGIN
  Def.Get(p);
  IF p.k # 5 THEN ok[1] := 0 END
END UseDef.
MODEOF
if "$OBC" build "$DRV/UseDef.mod" -I "$DRV" --out "$DRV" -o "$DRV/usedef" >/dev/null 2>&1; then
    status=0
    "$DRV/usedef" || status=$?
    check "pointer-to-a-record-in-another-module" "0" "$status"
    # The bug's signature is a name one side does not know: the header said
    # `Def__Anon_1` while the interface said `Node`, so neither may appear. The
    # header is the C back end's to write, whatever BACKEND says, so it is asked
    # for that way (the interface is written by either).
    mkdir -p "$DRV/defc"
    "$C_OBC" emit "$DRV/Def.mod" -I "$DRV" --out "$DRV/defc" >/dev/null 2>&1 || true
    check "neither-side-names-the-pointee-anonymously" "0" \
        "$(cat "$DRV/defc/Def.h" "$DRV/Def.sym" | grep -c 'Anon')"
    check "the-pointee-keeps-its-name-on-both-sides" "1" \
        "$(grep -c 'Def__Node' "$DRV/defc/Def.h" | grep -c '^[1-9]')"
else
    echo "FAIL pointer-to-a-record-in-another-module: did not build"
    fail=$((fail + 1))
fi

# 21. The second back end computes the C ABI layout (DESIGN section 16). The C
# back end emits structs and lets `cc` place the members, so `cc` is the
# authority and `layout.rs` makes the claim. `obc emit --layout` prints the
# computed table and stops. This test runs even though that back end does not
# lower records yet (PLAN.md W9.2a). Each fact has its own `check`, so a
# mismatch names the type and field.
#
# The C half names the driver directly. Under `BACKEND=clif`, `$OBC` is the
# wrapper that appends `--backend=clif`, and the later flag would win.
LAY=$(mktemp -d)
cat > "$DRV/Lay.mod" <<'MODEOF'
MODULE Lay;
  TYPE
    Node = POINTER TO Cell;
    Cell = RECORD key: INTEGER; ch: CHAR; r: REAL; next: Node END;
    Paired = RECORD (Cell) extra: INTEGER END;
    Tail = RECORD x: INTEGER; last: CHAR END;
    Empt = RECORD END;
    Cells = ARRAY 3 OF Cell;
    Nodes = ARRAY 4 OF Node;
    Words = ARRAY 5 OF INTEGER;
    Grid = ARRAY 3, 4 OF INTEGER;
  VAR a: Cell; b: Paired; c: Nodes; d: Words; e: Grid;
BEGIN
END Lay.
MODEOF
cat > "$LAY/probe.c" <<'PROBEEOF'
/* What `cc` says the emitted structs are. One printf per fact, spelling the
 * Oberon type and member names, so the two tables can be compared line for line
 * (a pointer type has no C typedef of its own: `Node` is `Cell *`). */
#include <stdio.h>
#include <stddef.h>
#include "Lay.h"
int main(void) {
    printf("Node size=%zu align=%zu\n", sizeof(Lay__Cell *), _Alignof(Lay__Cell *));
    printf("Cell size=%zu align=%zu\n", sizeof(Lay__Cell), _Alignof(Lay__Cell));
    printf("Cell.desc offset=%zu\n", offsetof(Lay__Cell, desc));
    printf("Cell.key offset=%zu\n", offsetof(Lay__Cell, key));
    printf("Cell.ch offset=%zu\n", offsetof(Lay__Cell, ch));
    printf("Cell.r offset=%zu\n", offsetof(Lay__Cell, r));
    printf("Cell.next offset=%zu\n", offsetof(Lay__Cell, next));
    printf("Paired size=%zu align=%zu\n", sizeof(Lay__Paired), _Alignof(Lay__Paired));
    printf("Paired.base offset=%zu\n", offsetof(Lay__Paired, base));
    printf("Paired.extra offset=%zu\n", offsetof(Lay__Paired, extra));
    printf("Tail size=%zu align=%zu\n", sizeof(Lay__Tail), _Alignof(Lay__Tail));
    printf("Tail.desc offset=%zu\n", offsetof(Lay__Tail, desc));
    printf("Tail.x offset=%zu\n", offsetof(Lay__Tail, x));
    printf("Tail.last offset=%zu\n", offsetof(Lay__Tail, last));
    printf("Empt size=%zu align=%zu\n", sizeof(Lay__Empt), _Alignof(Lay__Empt));
    printf("Empt.desc offset=%zu\n", offsetof(Lay__Empt, desc));
    printf("Cells size=%zu align=%zu\n", sizeof(Lay__Cells), _Alignof(Lay__Cells));
    printf("Nodes size=%zu align=%zu\n", sizeof(Lay__Nodes), _Alignof(Lay__Nodes));
    printf("Words size=%zu align=%zu\n", sizeof(Lay__Words), _Alignof(Lay__Words));
    printf("Grid size=%zu align=%zu\n", sizeof(Lay__Grid), _Alignof(Lay__Grid));
    return 0;
}
PROBEEOF
if "$C_OBC" emit "$DRV/Lay.mod" --backend=c --out "$LAY" >/dev/null 2>&1 &&
   $CC $CFLAGS -I "$LAY" -o "$LAY/probe" "$LAY/probe.c" 2>"$LAY/cc.log"; then
    "$LAY/probe" > "$LAY/cc.table"
    "$OBC" emit "$DRV/Lay.mod" --layout > "$LAY/computed.table" 2>/dev/null
    while IFS= read -r want; do
        [ -n "$want" ] || continue
        key=${want%% *}
        got=$(grep -F "$key " "$LAY/computed.table" || true)
        check "layout-$key" "$want" "${got:-<not in the computed table>}"
    done < "$LAY/cc.table"
else
    echo "FAIL layout-the-emitted-header-compiles: $(tail -1 "$LAY/cc.log" 2>/dev/null)"
    fail=$((fail + 1))
fi

# 22. Fixed scalar arrays on that layout, multi-dimensional ones included, on
# whichever back end `$OBC` is: module arrays are stored in a data object and
# local arrays in a zeroed frame slot. Each subscript is checked against its
# dimension, and the element is read and written using the layout stride.
cat > "$DRV/Grid.mod" <<'MODEOF'
MODULE Grid;
  VAR g: ARRAY 3, 4 OF INTEGER; t: ARRAY 2 OF INTEGER; ok: ARRAY 1 OF INTEGER;

  PROCEDURE Fill;
    VAR l: ARRAY 2, 2 OF INTEGER; i, j: INTEGER;
  BEGIN
    FOR i := 0 TO 1 DO FOR j := 0 TO 1 DO l[i, j] := i * 10 + j END END;
    g[0, 0] := l[1, 1];
    t[0] := g[0, 0];
    t[1] := g[1, 3] + g[2, 0]
  END Fill;
BEGIN
  g[1, 3] := 7; g[2, 0] := 5;
  Fill;
  IF t[0] # 11 THEN ok[1] := 0 END;
  IF t[1] # 12 THEN ok[1] := 0 END
END Grid.
MODEOF
if "$OBC" build "$DRV/Grid.mod" --out "$DRV" -o "$DRV/grid" >/dev/null 2>&1; then
    status=0
    "$DRV/grid" || status=$?
    check "array-multi-dimensional-indexing" "0" "$status"
else
    echo "FAIL array-multi-dimensional-indexing: did not build"
    fail=$((fail + 1))
fi

# A NIL pointer where a type is asked for: `NIL IS T` is false and a NIL type-CASE
# subject matches no label, so the default arm (or the located trap) answers. The C
# path read the descriptor through NIL and fault with no message. W9.2b found
# this gap while comparing the two back ends (W2.6).
runs_case nil-has-no-type 'MODULE NilT;
  TYPE R = RECORD END; S = RECORD (R) x: INTEGER END; P = POINTER TO R; PS = POINTER TO S;
  VAR p: P; i: INTEGER; ok: ARRAY 1 OF INTEGER;
BEGIN
  p := NIL;
  IF p IS PS THEN ok[1] := 0 END;
  IF ~(p IS PS) THEN i := 1 END;
  CASE p OF PS: ok[1] := 0 | : i := i + 1 END;
  IF i # 2 THEN ok[1] := 0 END
END NilT.
'

# 23. The ABI across back ends (DESIGN section 16). One program uses four
# modules, two back ends: `MixLib` and `MixC2` are C back-end objects, while
# `MixRoot` and `MixLeaf` are Cranelift objects. The host `cc` links them, so
# symbol names, argument/result conventions, and runtime calls must agree.
# The modules' ASSERTs check arguments into a C module, values out of a clif
# module, and values in both directions.
#
# `$C_OBC` is the driver itself, not the wrapper. Under BACKEND=clif the wrapper
# appends `--backend=clif` after the module, overriding the explicit flag each
# half needs.
#
# Emit each half into its own directory because `emit` writes the whole import
# graph. Asking for `MixRoot` under clif also writes a Cranelift `MixLib.o`,
# while the C back end's object is the one to link.
MIX="$DRV/mixed"
mkdir -p "$MIX/c" "$MIX/k" "$MIX/k2" "$MIX/c2"
if "$C_OBC" emit MixLib.mod  -I . --out "$MIX/c" >"$MIX/emit.log" 2>&1 &&
   "$C_OBC" emit MixRoot.mod -I "$MIX/c" -I . --out "$MIX/k" --backend=clif >>"$MIX/emit.log" 2>&1 &&
   "$C_OBC" emit MixLeaf.mod -I . --out "$MIX/k2" --backend=clif >>"$MIX/emit.log" 2>&1 &&
   "$C_OBC" emit MixC2.mod   -I "$MIX/k2" -I . --out "$MIX/c2" >>"$MIX/emit.log" 2>&1 &&
   "$CC" -c "$MIX/c/MixLib.c" -o "$MIX/c/MixLib.o" >>"$MIX/emit.log" 2>&1 &&
   "$CC" -c "$MIX/c2/MixC2.c" -o "$MIX/c2/MixC2.o" -I "$MIX/c" -I "$MIX/c2" >>"$MIX/emit.log" 2>&1; then
    echo "PASS mixed-backend-emits[one module per back end, per direction]"
    pass=$((pass + 1))
else
    echo "FAIL mixed-backend-emits:"
    sed -n '1,10p' "$MIX/emit.log"
    fail=$((fail + 1))
fi

# Objects from both back ends must export the same names for a module. Only the
# *defined* symbols are compared: each module can call its own runtime helpers.
# A differing or missing name would be an ABI disagreement, while an undefined
# name would fail the link below.
nm "$MIX/c/MixLib.o" | awk '$2 ~ /^[TDBR]$/ {print ($2 == "R" ? "D" : $2), $3}' | sort > "$MIX/sym.c"
nm "$MIX/k/MixLib.o" | awk '$2 ~ /^[TDBR]$/ {print ($2 == "R" ? "D" : $2), $3}' | sort > "$MIX/sym.k"
check "mixed-backend-same-symbols[init, load, Take, Twice, its variable]" "same" \
    "$(cmp -s "$MIX/sym.c" "$MIX/sym.k" && echo same || echo differ)"
check "mixed-backend-symbols-present" "yes" "$([ "$(wc -l < "$MIX/sym.k")" -ge 5 ] && echo yes || echo no)"

if "$CC" -o "$MIX/mixed" main_mixed.c "$MIX/c/MixLib.o" "$MIX/k/MixRoot.o" \
        "$MIX/k2/MixLeaf.o" "$MIX/c2/MixC2.o" "$MIX/c/oberon_runtime.c" \
        -I "$MIX/c" -I "$MIX/c2" -lm 2>"$MIX/link.log"; then
    echo "PASS mixed-backend-links"
    pass=$((pass + 1))
else
    echo "FAIL mixed-backend-links:"
    sed -n '1,10p' "$MIX/link.log"
    fail=$((fail + 1))
fi
mixed_status=0
"$MIX/mixed" || mixed_status=$?
check "mixed-backend-runs[clif calls C, C calls clif: argument, result and variable each way]" "0" "$mixed_status"

# 24. The second back end's optimisation level is the caller's (`-Oopt=LEVEL`,
#     Cranelift's own vocabulary). The default is `speed`, because the C path goes
#     through `gcc -O2` and Cranelift's own default is `none`; each level has to
#     build and run the same program, and a name that is not a level has to fail
#     the build rather than be ignored. `$C_OBC` and an explicit `--backend=clif`
#     keep this row the same under both BACKEND settings.
for level in none speed speed_and_size; do
    if "$C_OBC" build Hello.mod -I . --out "$DRV/opt-$level" -o "$DRV/opt-$level.bin" \
            --backend=clif -Oopt=$level 2>"$DRV/opt-$level.log"; then
        echo "PASS opt-level-builds[$level]"
        pass=$((pass + 1))
        opt_status=0
        "$DRV/opt-$level.bin" >/dev/null 2>&1 || opt_status=$?
        check "opt-level-runs[$level]" "0" "$opt_status"
    else
        echo "FAIL opt-level-builds[$level]: $(tail -1 "$DRV/opt-$level.log")"
        fail=$((fail + 1))
    fi
done
"$C_OBC" build Hello.mod -I . --out "$DRV/opt-bad" -o "$DRV/opt-bad.bin" \
    --backend=clif -Oopt=bogus >/dev/null 2>"$DRV/opt-bad.log" || true
check "opt-level-refuses-an-unknown-name[by name, not silently]" "1" \
    "$(grep -c 'names no optimisation level' "$DRV/opt-bad.log")"

# 25. Inlining must not change program behavior. The bug was that a spliced
# body's *data* named the caller's object: `cranelift-codegen` copies a callee's
# `GlobalValueData` verbatim, and its `User` name is an index into the
# *function's* name table. The two functions must declare the same data in the
# same order (PLAN.md W9.4). These modules preserve that shape:
# `InlStr` is the string literal that first printed the wrong text, `InlDesc`
# checks a record descriptor the caller never names, and `InlVar` tests the
# ordering hazard. Both functions declare both variables, but a map may iterate
# them in a different order, so impose the sequence. ASSERTs check each case,
# and the exit status is the verdict. `--backend=clif` selects the second
# back end's inlining, regardless of the BACKEND setting.
for inl_mod in InlStr InlDesc InlVar; do
    if "$C_OBC" build "$inl_mod.mod" -I . --out "$DRV/inl-$inl_mod" -o "$DRV/inl-$inl_mod.bin" \
            --backend=clif 2>"$DRV/inl-$inl_mod.log"; then
        echo "PASS inlining-preserves-meaning[$inl_mod]"
        pass=$((pass + 1))
        inl_status=0
        "$DRV/inl-$inl_mod.bin" >/dev/null 2>&1 || inl_status=$?
        check "inlining-runs[$inl_mod]" "0" "$inl_status"
    else
        echo "FAIL inlining-preserves-meaning[$inl_mod]: $(tail -1 "$DRV/inl-$inl_mod.log")"
        fail=$((fail + 1))
    fi
done

# 26. Programs written elsewhere, built and run here. `PrimeNumbers.mod` and
#     `Powers.mod` are from https://github.com/lboasso/oberonc (MIT), and the only
#     thing the port changes is how they reach the host's arguments: that compiler
#     provides `ARGNUM`/`ARGS` as built-ins, which neither the report nor another
#     implementation has. Here that information comes from the `Args` module
#     (DESIGN 11). `lib/` includes the `Out` module those programs expect. The
#     suite's `Out` is a different module with the same name, so search `lib`
#     first.
#
#     `Util.mod` is theirs unmodified. Both programs are checked by their output,
#     and the value checked is one only a correct run produces: the last power of
#     two that `Powers 8` writes, and the first prime `PrimeNumbers 50` prints.
PRG="-I ../lib -I . -I $DRV"
if "$C_OBC" build PrimeNumbers.mod $PRG --out "$DRV/prog-primes" -o "$DRV/prog-primes.bin" \
        --backend=clif 2>"$DRV/prog-primes.log"; then
    echo "PASS third-party-program-builds[PrimeNumbers]"
    pass=$((pass + 1))
    prog_out=$("$DRV/prog-primes.bin" 50 2>&1 | head -1)
    case "$prog_out" in
        *"    7   11   13"*) echo "PASS third-party-program-runs[PrimeNumbers 50]"; pass=$((pass + 1)) ;;
        *) echo "FAIL third-party-program-runs[PrimeNumbers 50]: got [$prog_out]"; fail=$((fail + 1)) ;;
    esac
else
    echo "FAIL third-party-program-builds[PrimeNumbers]: $(tail -1 "$DRV/prog-primes.log")"
    fail=$((fail + 1))
fi
if "$C_OBC" build Powers.mod $PRG --out "$DRV/prog-powers" -o "$DRV/prog-powers.bin" \
        --backend=clif 2>"$DRV/prog-powers.log"; then
    echo "PASS third-party-program-builds[Powers]"
    pass=$((pass + 1))
    prog_out=$("$DRV/prog-powers.bin" 8 2>&1 | tail -1)
    case "$prog_out" in
        *"0.0078125"*) echo "PASS third-party-program-runs[Powers 8]"; pass=$((pass + 1)) ;;
        *) echo "FAIL third-party-program-runs[Powers 8]: got [$prog_out]"; fail=$((fail + 1)) ;;
    esac
else
    echo "FAIL third-party-program-builds[Powers]: $(tail -1 "$DRV/prog-powers.log")"
    fail=$((fail + 1))
fi

# 27. Larger algorithm programs exercise recursive search and real-valued
#     iterated-function computation in end-to-end builds. Their output values
#     are fixed semantic results, not merely successful process exits.
for complex_mod in NQueens Fern; do
    case "$complex_mod" in
        NQueens) expected=" 92" ;;
        Fern) expected=" 100000  1018  85031  6919  7032  180  489  75  714  7632" ;;
    esac
    complex_out="$DRV/$complex_mod-$BACKEND_MODE"
    if "$OBC" build "$complex_mod.mod" -I ../lib -I . --out "$complex_out" \
            -o "$complex_out.bin" 2>"$complex_out.log"; then
        if complex_got=$("$complex_out.bin" 2>&1); then
            check "complex-program-runs[$complex_mod,$BACKEND_MODE]" "$expected" "$complex_got"
        else
            check "complex-program-runs[$complex_mod,$BACKEND_MODE]" "$expected" "process failed: $complex_got"
        fi
    else
        echo "FAIL complex-program-builds[$complex_mod,$BACKEND_MODE]: $(tail -1 "$complex_out.log")"
        fail=$((fail + 1))
    fi
done

echo "----"
echo "SUMMARY e2e: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
