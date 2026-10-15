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
# `BACKEND=clif` runs the same suite against the Cranelift back end, on the
# fragment it lowers (PLAN.md W9.1); the collector's block is C-only, because its
# oracle needs records and pointers.
#
# The default run is the standard suite: every seed that once found a defect
# (regressions.txt), as a value oracle and as a trap oracle, then a block for
# coverage, and then the same value oracle with the collector forced on. It is
# sized to sit beside the other suites in the gate; the slower variants are one
# flag away — `SAN=1` adds the sanitizers to whatever runs, and
# SEEDS/BASE/COVER/ADVCOVER/GCCOVER select ranges and sizes. Either way a seed
# reproduces a program exactly, on any machine.
set -e
cd -- "$(dirname -- "$0")"

# BACKEND=clif builds every program with the Cranelift back end instead of the C
# one (PLAN.md W9.1). That back end lowers the corpus's scalar fragment --
# INTEGER arithmetic and the shifts, the control flow, procedures, recursion and
# the `fail[1]` self-checks the oracle traps through -- so `obgen` generates that
# fragment for it and the two blocks whose oracle needs a record or a pointer
# (the collector's heap probe, W10.2) are not run. Everything else is the same
# oracle: the regression list, its trap and dialect variants, and a coverage
# block, with the numbers below stated in the summary.
BACKEND=${BACKEND:-c}

OBG=${OBG:-../target/debug/obgen}
OBC=${OBC:-../target/debug/obc}
# One directory per run: a failing seed's module is kept there for triage, so two
# runs sharing one path clobber each other's artifacts and can turn a good run into
# a spurious failure (found by running the corpus on two back ends at once). The
# path is printed with every failure, and the parent directory is gitignored.
WORK=${WORK:-findings/run-$$}
LIST=${LIST:-regressions.txt}
COVER=${COVER:-30}
ADVCOVER=${ADVCOVER:-10}
GCCOVER=${GCCOVER:-10}
BASE=${BASE:-1}

# SAN=1 builds every generated program with the sanitizers the golden suite uses,
# which makes a clean run evidence about the *emitted* code rather than about the
# program's own arithmetic (PLAN.md W7.5). Leak detection stays on: what is still
# on the heap when the program ends is what it still holds, because the runtime
# collects once at exit (W10.2), so a leak here is a finding rather than a
# language property.
#
# `--lower` renders the same programs with reserved words in lower case and builds
# them in the `-Odialect=lower` dialect (PLAN.md W8.10), so the one extension that
# had to be gated is covered end to end by the corpus rather than only by
# hand-written cases. The values are the same, which is the point: a dialect that
# changed them would be a different language.
#
# The adversarial mode appends one deliberate violation per program (an index
# past the end, a NIL dereference, a guard that cannot hold, a computed zero
# divisor, a false assertion) and the run must produce the exact trap line --
# message *and* source byte. That is the only test of those paths that does not
# come from a hand-written case.
#
# GC=1 runs a block with the collector forced on every allocation, through
# `OB_GC_STRESS=1` in the environment of `obgen` and so of every program it
# builds and runs (PLAN.md W10.2). The oracle is the programs' own checks: each
# generated program now has a heap probe that keeps a chain alive per kind of
# root -- in a local, in a module variable, and inside a module record -- and
# checks it after some churn, so a collector that freed a live object makes the
# seed fail rather than pass quietly. Such a block always builds with the
# sanitizers, SAN=1 or not: a freed object whose memory has not been reused yet
# still reads correctly, so the value checks alone cannot see the one thing the
# collector must never do, which is exactly how the address sanitizer earned its
# place here while W10.2 was being written.
SAN_FLAGS=(
    --cflag -fsanitize=address,undefined
    --cflag -fno-sanitize-recover=all
    --cflag -g
)
FLAGS=()
if [ -n "${SAN:-}" ]; then
    FLAGS=("${SAN_FLAGS[@]}")
fi
export ASAN_OPTIONS=detect_leaks=1
export UBSAN_OPTIONS=print_stacktrace=1:halt_on_error=1

status=0
run() {
    local -a flags=("${FLAGS[@]}" "--backend=$BACKEND")
    if [ -n "${GC:-}" ]; then
        export OB_GC_STRESS=1
        flags+=("${SAN_FLAGS[@]}")
    fi
    "$OBG" --obc "$OBC" --work "$WORK" "${flags[@]}" "$@" || status=$?
}

if [ -n "${SEEDS:-}" ]; then
    run --seeds "$SEEDS" --base "$BASE" "$@"
    echo "SUMMARY fuzz: seeds $BASE..$((BASE + SEEDS - 1))${SAN:+ under sanitizers}, exit $status"
elif [ "$BACKEND" = clif ]; then
    run --seed-list "$LIST" "$@"                    # the value oracle
    echo "----"
    run --seed-list "$LIST" --adversarial "$@"      # the trap oracle
    run --seed-list "$LIST" --lower "$@"            # the same programs, lower-case dialect
    echo "----"
    run --seeds "$COVER" --base "$BASE" "$@"        # coverage
    echo "----"
    run --seeds "$ADVCOVER" --base "$BASE" --adversarial "$@"
    echo "SUMMARY fuzz: clif (values, traps, lower-case dialect) + $COVER coverage + $ADVCOVER adversarial, exit $status"
else
    run --seed-list "$LIST" "$@"                    # the value oracle
    echo "----"
    run --seed-list "$LIST" --adversarial "$@"      # the trap oracle
    run --seed-list "$LIST" --lower "$@"            # the same programs, lower-case dialect
    echo "----"
    run --seeds "$COVER" --base "$BASE" "$@"        # coverage
    echo "----"
    run --seeds "$ADVCOVER" --base "$BASE" --adversarial "$@"
    echo "----"
    GC=1 run --seed-list "$LIST" "$@"               # the value oracle, under GC stress
    echo "----"
    GC=1 run --seeds "$GCCOVER" --base "$BASE" "$@" # and fresh programs under it
    echo "SUMMARY fuzz: regression list (values, traps, stress, lower-case dialect) + $COVER coverage + $ADVCOVER adversarial + $GCCOVER under stress${SAN:+ under sanitizers}, exit $status"
fi
exit $status
