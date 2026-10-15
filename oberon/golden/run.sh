#!/usr/bin/env bash
# golden -- the C that DESIGN.md 9.5 (expressions), 9.6 (statements), 9.7
# (procedures/calls) and 9.8 (module init / exported interface) say the back end
# must emit, written out by hand and executed, plus the C half of the §8.3 claim
# that the front end's constant folder and the emitted C compute the same values
# (constexpr.c writes constexpr.tsv, constexpr_check.c verifies it).
#
# Two builds over the same sources:
#   1. the design's own line (DESIGN 9.1): -std=c11 -O2 -fwrapv -Wall -Wextra -Werror
#   2. the same, under -fsanitize=address,undefined, which must be clean: it is
#      the evidence that rt_charcmp's bounded comparison never reads past either
#      operand (the operands are heap blocks with redzones behind them).
# One "PASS <case>" / "FAIL <case>: <detail>" line per case and a final summary
# come from the binary; the script exits non-zero if anything failed.
set -e
cd -- "$(dirname -- "$0")"

CC=${CC:-gcc}
CFLAGS="-std=c11 -O2 -fwrapv -Wall -Wextra -Werror -Wno-overflow"
SUITE_SRCS="harness.c lower.c constexpr_check.c Ob.c Out.c Main.c"
TABLE_SRCS="constexpr.c"

PLAIN_LOG=$(mktemp)
SAN_LOG=$(mktemp)
GEN_LOG=$(mktemp)
PLAIN_TSV=$(mktemp)
# Binaries are built outside the source tree: this directory is documentation as
# much as it is a test fixture.
BINDIR=$(mktemp -d)
trap 'rm -f "$PLAIN_LOG" "$SAN_LOG" "$GEN_LOG" "$PLAIN_TSV"; rm -rf "$BINDIR"' EXIT

# --- build 1 ----------------------------------------------------------------
$CC $CFLAGS -o "$BINDIR/golden" $SUITE_SRCS
$CC $CFLAGS -o "$BINDIR/constexpr" $TABLE_SRCS

# DESIGN §8.3: the constant-expression table the folder is diffed against.  It
# is written next to this script, and the suite's constexpr-table case reads it.
"$BINDIR/constexpr"
cp constexpr.tsv "$PLAIN_TSV"

# DESIGN section 7 / 9.8: an exported variable is read-only for the importing
# module.  C cannot express that (see Out.h), so only the mechanical half of the
# rule can be checked from C: the importing translation unit never assigns to it.
if grep -nE '(^|[^=!<>[:alnum:]_])Out__(done|W)[[:space:]]*=[^=]' Main.c; then
  echo "FAIL readonly-export-import-side: Main.c assigns to an exported Out variable"
  exit 1
else
  echo "PASS readonly-export-import-side-no-assignment[C cannot enforce it: front-end rule; checked by grep of the importing file]"
fi

set -o pipefail
"$BINDIR/golden" | tee "$PLAIN_LOG"

# --- build 2: address + undefined behaviour sanitizers ----------------------
$CC $CFLAGS -fsanitize=address,undefined -fno-sanitize-recover=all -g -o "$BINDIR/golden-asan" $SUITE_SRCS
$CC $CFLAGS -fsanitize=address,undefined -fno-sanitize-recover=all -g -o "$BINDIR/constexpr-asan" $TABLE_SRCS
echo "--- second build: -fsanitize=address,undefined -fno-sanitize-recover=all ---"

# the table generator first, so the sanitized suite reads the file the sanitized
# writer produces; its output must be byte-identical to the plain build's
if ! ASAN_OPTIONS=detect_leaks=1 UBSAN_OPTIONS=print_stacktrace=1:halt_on_error=1 \
     "$BINDIR/constexpr-asan" >"$GEN_LOG" 2>&1; then
  echo "FAIL asan-ubsan-clean: the sanitized constexpr exited non-zero"
  cat "$GEN_LOG"
  exit 1
fi
cat "$GEN_LOG"
if ! diff -q "$PLAIN_TSV" constexpr.tsv >/dev/null; then
  echo "FAIL constexpr-tsv-stable: the two builds write different constexpr.tsv"
  exit 1
fi

if ! ASAN_OPTIONS=detect_leaks=1 UBSAN_OPTIONS=print_stacktrace=1:halt_on_error=1 \
     "$BINDIR/golden-asan" >"$SAN_LOG" 2>&1; then
  echo "FAIL asan-ubsan-clean: the sanitized suite exited non-zero"
  cat "$SAN_LOG"
  exit 1
fi
cat "$SAN_LOG"
if grep -qE 'AddressSanitizer|UndefinedBehaviorSanitizer|LeakSanitizer|runtime error' "$GEN_LOG" "$SAN_LOG"; then
  echo "FAIL asan-ubsan-clean: sanitizer report in the log"
  exit 1
fi
if ! diff <(grep -E '^(PASS|FAIL) ' "$PLAIN_LOG") <(grep -E '^(PASS|FAIL) ' "$SAN_LOG") >/dev/null; then
  echo "FAIL asan-ubsan-same-cases: the two builds do not report the same case lines"
  exit 1
fi
echo "PASS asan-ubsan-clean[-fsanitize=address,undefined: no report in either program, identical case lines, identical constexpr.tsv]"
