#!/usr/bin/env bash
# fuzz -- random Oberon-07 programs, each of which knows what it must compute.
#
# The generator (src/bin/obgen.rs) writes a module and the value every variable
# must have; the module ends with one check per variable, and a failing check
# traps. So a seed fails if the compiler rejects a well-formed program, if the
# emitted C does not compile under the documented build line, or if the program
# computes something else -- with no output expected from the program and no
# expectation taken from the compiler.
#
# The default run is the standard suite: every seed that once found a defect
# (regressions.txt), as a value oracle and as a trap oracle, then a block for
# coverage. It is sized to sit beside the other suites in the gate; the slower
# variants are one flag away — `SAN=1` adds the sanitizers to whatever runs, and
# SEEDS/BASE/COVER/ADVCOVER select ranges and sizes. Either way a seed reproduces
# a program exactly, on any machine.
set -e
cd -- "$(dirname -- "$0")"

OBG=${OBG:-../target/debug/obgen}
OBC=${OBC:-../target/debug/obc}
WORK=${WORK:-findings}
LIST=${LIST:-regressions.txt}
COVER=${COVER:-30}
ADVCOVER=${ADVCOVER:-10}
BASE=${BASE:-1}

# SAN=1 builds every generated program with the sanitizers the golden suite uses,
# which makes a clean run evidence about the *emitted* code rather than about the
# program's own arithmetic (PLAN.md W7.5). Leak detection stays on: the fragment
# allocates nothing yet, so a leak would be a finding rather than a language
# property -- when it does allocate, W10 decides what a leak means.
#
# The adversarial mode appends one deliberate violation per program (an index
# past the end, a NIL dereference, a guard that cannot hold, a computed zero
# divisor, a false assertion) and the run must produce the exact trap line --
# message *and* source byte. That is the only test of those paths that does not
# come from a hand-written case.
FLAGS=()
if [ -n "${SAN:-}" ]; then
    FLAGS=(--cflag -fsanitize=address,undefined --cflag -fno-sanitize-recover=all --cflag -g)
    export ASAN_OPTIONS=detect_leaks=1
    export UBSAN_OPTIONS=print_stacktrace=1:halt_on_error=1
fi

status=0
run() { "$OBG" --obc "$OBC" --work "$WORK" "${FLAGS[@]}" "$@" || status=$?; }

if [ -n "${SEEDS:-}" ]; then
    run --seeds "$SEEDS" --base "$BASE" "$@"
    echo "SUMMARY fuzz: seeds $BASE..$((BASE + SEEDS - 1))${SAN:+ under sanitizers}, exit $status"
else
    run --seed-list "$LIST" "$@"                    # the value oracle
    echo "----"
    run --seed-list "$LIST" --adversarial "$@"      # the trap oracle
    echo "----"
    run --seeds "$COVER" --base "$BASE" "$@"        # coverage
    echo "----"
    run --seeds "$ADVCOVER" --base "$BASE" --adversarial "$@"
    echo "SUMMARY fuzz: regression list (values, traps) + $COVER coverage + $ADVCOVER adversarial${SAN:+ under sanitizers}, exit $status"
fi
exit $status
