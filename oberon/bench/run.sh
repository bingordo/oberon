#!/usr/bin/env bash
# A performance *report*, not a suite: it prints numbers and never fails a run.
# Timings are noisy and machine-dependent, so there is nothing here to assert.
#
# Two shapes, because they answer different questions. `Arith.mod` is a tight
# integer loop with no calls and no arrays -- straight-line code generation, where
# the second back end is within a third of `gcc -O2`. `CallArray.mod` calls a
# procedure and indexes an array once per iteration: `gcc` inlines the call and
# vectorises the loop, neither of which Cranelift does here, so the gap there is
# about *structure* -- and with the runtime checks on it widens, because the C
# back end's checks are an inline compare to a cold trap while the second's are a
# call to `rt_index`/`rt_rt` per access. Both are built with the driver's own
# flags, so the comparison is `-O2` against `opt_level=speed` with the same runtime.
set -e
cd -- "$(dirname -- "$0")"
OBC=${OBC:-../target/debug/obc}
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
printf '%-14s %8s %10s %10s %8s\n' module config c clif ratio
for m in Arith CallArray; do
    for mode in checks nochecks; do
        case $mode in
            checks)   flags="" ;;
            nochecks) flags="-Ochecks=none" ;;
        esac
        "$OBC" build "$m.mod" --out "$WORK/$m-c-$mode" --keep -o "$WORK/$m-c" $flags >/dev/null 2>&1
        "$OBC" build "$m.mod" --out "$WORK/$m-k-$mode" --keep -o "$WORK/$m-k" --backend=clif $flags >/dev/null 2>&1
        read -r c k < <(python3 - "$WORK/$m-c" "$WORK/$m-k" <<'PY'
import subprocess, sys, time
out=[]
for exe in sys.argv[1:3]:
    best=min((lambda: (lambda s: (subprocess.run([exe], check=True), time.perf_counter()-s)[1])(time.perf_counter()))() for _ in range(3))
    out.append(best)
print(f"{out[0]:.4f} {out[1]:.4f}")
PY
)
        printf '%-22s %10s %9ss %8ss %7s\n' "$m" "$mode" "$c" "$k" \
            "$(python3 -c "print(f'{$k/$c:.2f}x')")"
    done
done
echo
echo "gcc: $(${CC:-gcc} --version | head -1)"
