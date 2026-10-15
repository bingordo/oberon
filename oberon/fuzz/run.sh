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
# (regressions.txt), then a block for coverage. SEEDS/BASE select one contiguous
# range instead, for an ad-hoc run; either way a seed reproduces on any machine.
set -e
cd -- "$(dirname -- "$0")"

OBG=${OBG:-../target/debug/obgen}
OBC=${OBC:-../target/debug/obc}
WORK=${WORK:-findings}
LIST=${LIST:-regressions.txt}
COVER=${COVER:-100}
BASE=${BASE:-1}

status=0
if [ -n "${SEEDS:-}" ]; then
    "$OBG" --seeds "$SEEDS" --base "$BASE" --obc "$OBC" --work "$WORK" "$@" || status=$?
    echo "SUMMARY fuzz: seeds $BASE..$((BASE + SEEDS - 1)), exit $status"
else
    "$OBG" --seed-list "$LIST" --obc "$OBC" --work "$WORK" "$@" || status=$?
    echo "----"
    "$OBG" --seeds "$COVER" --base "$BASE" --obc "$OBC" --work "$WORK" "$@" || status=$?
    echo "SUMMARY fuzz: regression list + seeds $BASE..$((BASE + COVER - 1)), exit $status"
fi
exit $status
