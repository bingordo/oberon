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
# SEEDS/BASE select the run; the default is short enough for the gate, and any
# seed is reproducible on any machine.
set -e
cd -- "$(dirname -- "$0")"

OBG=${OBG:-../target/debug/obgen}
OBC=${OBC:-../target/debug/obc}
SEEDS=${SEEDS:-25}
BASE=${BASE:-1}
WORK=${WORK:-findings}

status=0
"$OBG" --seeds "$SEEDS" --base "$BASE" --obc "$OBC" --work "$WORK" "$@" || status=$?

echo "SUMMARY fuzz: seeds $BASE..$((BASE + SEEDS - 1)), exit $status"
exit $status
