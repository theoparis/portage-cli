#!/bin/bash
# Compare em -p / em -s against emerge on package-set parity and wall time.
#
# Usage: benchmarks/bench-em-vs-emerge.sh [path-to-em]
#   EM=target/release/em            binary under test (arg overrides)
#   RUNS=5                          hyperfine runs per timing entry
#   SKIP_TIMING=1                   parity check only
#
# Sets are compared on the versioned package list of the merge plan; both
# tools print the autounmask-adjusted preview graph, so counts should be
# identical (multi-target is the known exception: emerge's backtracking can
# stop partway through a USE-adjustment cascade that em completes).

set -u
EM=${1:-${EM:-target/release/em}}
RUNS=${RUNS:-5}
cd "$(dirname "$0")/.." || exit 1

if [ ! -x "$EM" ]; then
    echo "error: $EM not found (cargo build --release first)" >&2
    exit 1
fi

SINGLE_TARGETS=(
    dev-qt/qtbase
    app-text/texlive-core
    www-client/firefox
    dev-qt/qtwebengine
    mail-client/thunderbird
    app-office/libreoffice
    app-emulation/qemu
)
# Overlay targets are skipped silently when the repo isn't configured.
OVERLAY_TARGETS=(
    cross-riscv64-unknown-elf/gcc
)
MULTI="app-office/libreoffice dev-qt/qtwebengine mail-client/thunderbird app-emulation/qemu www-client/firefox"

extract() { grep -oE '^\[[^]]*\] [a-z0-9-]+/[A-Za-z0-9._+-]+' | awk '{print $NF}' | sort -u; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0

echo "== package-set parity (em -p vs emerge -p)"
for pkg in "${SINGLE_TARGETS[@]}" "${OVERLAY_TARGETS[@]}"; do
    emerge -p "$pkg" 2>/dev/null | extract > "$tmp/emerge.txt"
    if [ ! -s "$tmp/emerge.txt" ]; then
        echo "   $pkg: skipped (emerge resolves nothing — repo not configured?)"
        continue
    fi
    "$EM" -p "$pkg" 2>/dev/null | extract > "$tmp/em.txt"
    diffs=$(diff "$tmp/emerge.txt" "$tmp/em.txt" | grep -c '^[<>]')
    printf '   %-40s emerge=%-4s em=%-4s diffs=%s\n' \
        "$pkg" "$(wc -l < "$tmp/emerge.txt")" "$(wc -l < "$tmp/em.txt")" "$diffs"
    [ "$diffs" -ne 0 ] && fail=1
done

echo "== multi-target set (informational: cascade-tail divergence expected)"
emerge -p $MULTI 2>/dev/null | extract > "$tmp/emerge.txt"
"$EM" -p $MULTI 2>/dev/null | extract > "$tmp/em.txt"
echo "   emerge=$(wc -l < "$tmp/emerge.txt") em=$(wc -l < "$tmp/em.txt")"
diff "$tmp/emerge.txt" "$tmp/em.txt" | grep '^[<>]' | sed 's/^/   /'

if [ "${SKIP_TIMING:-0}" != 1 ]; then
    echo "== timing (hyperfine, $RUNS runs)"
    hyperfine -w 1 -r "$RUNS" --ignore-failure \
        "$EM -p www-client/firefox" "emerge -p www-client/firefox" \
        2>/dev/null | grep -E "Benchmark|Time|faster"
    hyperfine -w 1 -r "$RUNS" --ignore-failure \
        "$EM -p app-office/libreoffice" "emerge -p app-office/libreoffice" \
        2>/dev/null | grep -E "Benchmark|Time|faster"
    hyperfine -w 1 -r "$RUNS" --ignore-failure \
        "$EM -p $MULTI" "emerge -p $MULTI" \
        2>/dev/null | grep -E "Benchmark|Time|faster"
    hyperfine -w 1 -r "$RUNS" \
        "$EM -s gcc" "emerge -s gcc" \
        2>/dev/null | grep -E "Benchmark|Time|faster"
fi

if [ "$fail" -ne 0 ]; then
    echo "RESULT: parity FAILED" >&2
    exit 1
fi
echo "RESULT: parity OK"
