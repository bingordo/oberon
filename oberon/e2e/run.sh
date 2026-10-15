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
# suppressed; `--ferror-limit 0` means no limit (PLAN.md W5.2).
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
# spellings (report §6.4 makes using NIL an error; the design traps rather than
# faulting: R21, computed as a fault before this check existed).
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

# An interface that cannot be read is explained only when the build needs it: an
# unrelated file on the search path must not be reported (PLAN.md W3.2).
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
# copy from the guarded view — reading a derived *field* through the view is the
# static-analysis problem R15 records, where gcc sees the read is out of the
# allocation's bounds and -Werror rejects the program although the guard's trap
# fires first. The *pointer* spelling is the pre-existing `failed-guard-traps` row.
trap_case guard-record-spelling 'MODULE T4;
  TYPE R = RECORD END; S = RECORD (R) f: INTEGER END; P = POINTER TO R;
  VAR p: P; s: S;
BEGIN
  NEW(p);
  s := p(S)
END T4.
' 'oberon: trap: type guard failed at byte 124'
# The *record* spelling of a failing guard, as an assignment target: the write
# must compile under the documented line and trap with the guard's offset. It used
# to be rejected, because gcc folds an inlined `malloc(sizeof(R))` and could then
# prove the write lay outside the allocation (DESIGN R15, PLAN.md W2.2).
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
# ... and the same program under clang, which the same design row promises.
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
if "$OBC" build "$DRV/Checks.mod" --out "$DRV" -o "$DRV/checks-on" >/dev/null 2>&1 &&
   "$OBC" build "$DRV/Checks.mod" -Ochecks=n --out "$DRV" -o "$DRV/checks-off" >/dev/null 2>&1; then
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
    # Every check has a name, and a named set emits exactly those (W8.3).
    printf 'MODULE Named;\n  TYPE R = RECORD x: INTEGER END; P = POINTER TO R;\n  VAR a: ARRAY 100 OF INTEGER; p: P; n: INTEGER;\nBEGIN\n  n := a[150]; p.x := 1\nEND Named.\n' > "$DRV/Named.mod"
    "$OBC" emit "$DRV/Named.mod" -Ochecks=nil --out "$DRV" >/dev/null 2>&1
    check "named-checks-emit-only-that-check" "1" \
        "$(grep -c 'rt_nil' "$DRV/Named.c")"
    check "named-checks-leave-the-others-out" "0" \
        "$(grep -c 'rt_index' "$DRV/Named.c")"
    "$OBC" emit "$DRV/Named.mod" -Ochecks=index --out "$DRV" >/dev/null 2>&1
    check "named-checks-can-turn-one-back-on" "1" \
        "$(grep -c 'rt_index' "$DRV/Named.c")"
    # And an unknown name is a diagnostic rather than a silent default.
    if "$OBC" emit "$DRV/Named.mod" -Ochecks=banana --out "$DRV" 2>"$DRV/banana.log"; then
        echo "FAIL unknown-check-name-is-refused: the flag was accepted"
        fail=$((fail + 1))
    else
        check "unknown-check-name-is-refused" "1" "$(grep -c 'names no check' "$DRV/banana.log")"
    fi
else
    echo "FAIL checks-flag: the module did not build either way"
    fail=$((fail + 1))
fi

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

# `NEW` failing is *specified* to yield NIL (report §6.4), so the hook makes it
# observable: the program traps itself if the allocation did not fail.
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
# no node of a live chain was reclaimed. Three modes, one answer: collection
# forced on every allocation, collection triggered by the limit, and collection
# idle (`OB_GC_STRESS` and `OB_GC_LIMIT` unset) -- which is also the evidence
# that a program that never needs the collector pays nothing for it.
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
    # 1000 rounds of 20 nodes: 20000 allocations, each round's chain dead before
    # the next one starts, so the live set is one 20-node chain -- 960 bytes, a
    # 24-byte header in front of each 24-byte node -- and the whole run would
    # need 960000. 210000 is the sum of the values 1..20, a thousand times over.
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

# A live set larger than the limit: `NEW` yields NIL (report §6.4) and the
# attempt ends there -- a collection that frees nothing must not loop -- while
# every node that did fit is still there, which is what the count says.
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

# A pointer field the *base* of the object's type declares: the object's own
# descriptor is the extension's, so the map has to reach the field one level in,
# through `offsetof(S, base) + offsetof(R, q)`. `head.q` is the only reference to
# the leaf object, so a map that missed the inherited field loses it.
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

# A designator narrowed by a type-CASE arm or by a guard stays *assignable*. The
# cast that gives a derived field its layout is not itself an lvalue, so the plain
# spelling is used wherever a store is needed, and only where the narrowed name is
# the whole designator (DESIGN R29, PLAN.md W7.6).
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

# 10. report §11.1's SYSTEM (PLAN.md W4.1): the six entries of the table in one
#     program whose every step is an assertion (Sys.mod), the import that resolves
#     to a builtin, the entries the report lists as implementation-specific
#     refused *by name*, and the promise that `-Ochecks` has nothing to do here.
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
# portable (PLAN.md W4.1 step 4) -- and SYSTEM has no `.h` to include.
check "system-import-is-recorded-in-the-interface" "IMPORT SYSTEM" "$(sed -n 3p "$DRV/Sys.sym")"
check "system-has-no-header-to-include" "0" "$(grep -c 'SYSTEM\.h' "$DRV/Sys.h")"

# The module is *the compiler's*: a `SYSTEM.mod`/`SYSTEM.sym` on the search path
# is never read, compiled or linked, so a broken pair there cannot reach a build
# (a user supply would be a different language, and a program that names SYSTEM
# in its import list must get the report's entries).
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
if "$OBC" emit "$DRV/SysChk.mod" --out "$DRV/chk-on" >/dev/null 2>&1 &&
   "$OBC" emit "$DRV/SysChk.mod" --out "$DRV/chk-off" -Ochecks=n >/dev/null 2>&1; then
    same=0
    cmp -s "$DRV/chk-on/SysChk.c" "$DRV/chk-off/SysChk.c" || same=1
    check "system-emits-the-same-c-under--Ochecks=n" "0" "$same"
    check "system-emits-no-runtime-check" "0" \
        "$(grep -c 'rt_index\|rt_nil\|rt_guard_is' "$DRV/chk-on/SysChk.c")"
else
    echo "FAIL system-checks-flag: the module did not emit either way"
    fail=$((fail + 1))
fi

# Every refusal is located, and the extras are refused by the name the program
# used (`SYSTEM.ADC is not implemented`, not a silent no-op).
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

# `MIN(T)`/`MAX(T)` (W8.2) and `CASE … ELSE` (W8.1): the extremes are constants,
# wherever they appear, and `ELSE` is report §9.5's default arm under a keyword
# LALR(1) can parse.
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
# Two defaults are one answer too many, and the message says so.
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
    CASE x OF 1: RETURN 10 | 2: RETURN 20 END;
    RETURN 7
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
    # `Def__Anon_1` while the interface said `Node`, so neither may appear.
    check "neither-side-names-the-pointee-anonymously" "0" \
        "$(cat "$DRV/Def.h" "$DRV/Def.sym" | grep -c 'Anon')"
    check "the-pointee-keeps-its-name-on-both-sides" "1" \
        "$(grep -c 'Def__Node' "$DRV/Def.h" | grep -c '^[1-9]')"
else
    echo "FAIL pointer-to-a-record-in-another-module: did not build"
    fail=$((fail + 1))
fi

echo "----"
echo "SUMMARY e2e: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
