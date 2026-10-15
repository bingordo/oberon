#!/usr/bin/env bash
set -e
cd -- "$(dirname -- "$0")"
# Built outside the source tree: this directory is documentation as much as it
# is a test fixture, and no binary belongs in it.
BIN="$(mktemp -d)/smoke"
trap 'rm -rf "$(dirname "$BIN")"' EXIT
gcc -std=c11 -O2 -fwrapv -Wall -Wextra -Werror -o "$BIN" smoke.c
"$BIN"
