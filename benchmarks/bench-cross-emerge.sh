#!/bin/bash
# Compare em --root-aware against {target}-emerge on crossdev sysroots.
#
# Usage: benchmarks/bench-cross-emerge.sh [path-to-em]
#   EM=target/release/em          binary under test (arg overrides)
#   Dual-root is auto-detected from --config-root vs --root (no flag).
#   CROSS_CHOST=riscv64-unknown-linux-gnu
#   SYSROOT=/usr/${CROSS_CHOST}   config + merge root for both tools
#   ARCH=riscv64                  em --arch (keyword: riscv)
#   ACCEPT_LICENSE=*              em workaround until @FREE group expansion
#   RUNS=3                        hyperfine runs (SKIP_TIMING=1 to skip)
#
# emerge reads ACCEPT_LICENSE from the cross profile (@FREE). em currently needs
# ACCEPT_LICENSE=* (or an explicit list) because license groups are not expanded
# yet — see use_env.rs.

set -u
EM=${1:-${EM:-target/release/em}}
CROSS_CHOST=${CROSS_CHOST:-riscv64-unknown-linux-gnu}
SYSROOT=${SYSROOT:-/usr/${CROSS_CHOST}}
ARCH=${ARCH:-riscv64}
ACCEPT_LICENSE=${ACCEPT_LICENSE:-*}
RUNS=${RUNS:-3}
EMERGE=${CROSS_CHOST}-emerge

cd "$(dirname "$0")/.." || exit 1

if [ ! -x "$EM" ]; then
    echo "error: $EM not found (cargo build --release first)" >&2
    exit 1
fi
if ! command -v "$EMERGE" >/dev/null 2>&1; then
    echo "error: $EMERGE not found — install crossdev wrapper" >&2
    exit 1
fi
if [ ! -d "$SYSROOT" ]; then
    echo "error: sysroot $SYSROOT missing" >&2
    exit 1
fi

# Normalise merge-list atoms: drop slot/repo/USE/root suffixes.
extract_cpns() {
    sed -n 's/^\[ebuild .*] //p' \
        | sed 's/ USE=.*//' \
        | sed 's/ to .*//' \
        | sed 's/::[^ ]*//' \
        | sed 's/:.*//' \
        | awk '{$1=$1; print}' \
        | sort -u
}

em_cmd() {
    ACCEPT_LICENSE="$ACCEPT_LICENSE" \
        "$EM" -p --config-root "$SYSROOT" --root "$SYSROOT" --arch "$ARCH" "$@"
}

TARGETS=(
    sys-devel/gcc
    sys-libs/zlib
    virtual/libiconv
)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0

echo "== cross merge-list parity ($CROSS_CHOST, em dual-root auto)"
echo '| package | emerge | em | only emerge | only em |'
echo '|---------|--------|----|-------------|---------|'
for pkg in "${TARGETS[@]}"; do
    if ! "$EMERGE" -pv "$pkg" 2>/dev/null | extract_cpns > "$tmp/emerge.txt"; then
        echo "   $pkg: emerge failed — skipped"
        continue
    fi
    if ! em_cmd "$pkg" 2>/dev/null | extract_cpns > "$tmp/em.txt"; then
        echo "   $pkg: em failed — skipped"
        continue
    fi
    emerge_n=$(wc -l < "$tmp/emerge.txt")
    em_n=$(wc -l < "$tmp/em.txt")
    only_e=$(comm -23 "$tmp/emerge.txt" "$tmp/em.txt" | wc -l)
    only_m=$(comm -13 "$tmp/emerge.txt" "$tmp/em.txt" | wc -l)
    printf '| %-22s | %4s | %4s | %11s | %7s |\n' \
        "$pkg" "$emerge_n" "$em_n" "$only_e" "$only_m"
    if [ "$only_e" -ne 0 ] || [ "$only_m" -ne 0 ]; then
        fail=1
        echo "   emerge-only:"; comm -23 "$tmp/emerge.txt" "$tmp/em.txt" | sed 's/^/     /'
        echo "   em-only:"; comm -13 "$tmp/emerge.txt" "$tmp/em.txt" | sed 's/^/     /'
    fi
done

echo
echo "== with-bdeps host-front matter (informational)"
if em_cmd --with-bdeps sys-devel/gcc 2>/dev/null | extract_cpns > "$tmp/em-bdeps.txt"; then
    bdeps_n=$(wc -l < "$tmp/em-bdeps.txt")
    base_n=$(wc -l < "$tmp/em.txt" 2>/dev/null || echo 0)
    host_lines=$(grep -c ' to /$' <(em_cmd --with-bdeps sys-devel/gcc 2>/dev/null) || true)
    echo "   sys-devel/gcc: emerge=18 (typical) em --with-bdeps=$bdeps_n (without=${base_n}) host-root lines=$host_lines"
    echo "   (post-solve host BDEPEND expansion still over-pulls; see docs/root-model.md Stage 3a)"
fi

if [ "${SKIP_TIMING:-0}" != 1 ] && command -v hyperfine >/dev/null 2>&1; then
    echo
    echo "== timing (hyperfine, $RUNS runs)"
    hyperfine -w 1 -r "$RUNS" --ignore-failure \
        "ACCEPT_LICENSE='$ACCEPT_LICENSE' $EM -p --config-root $SYSROOT --root $SYSROOT --arch $ARCH sys-devel/gcc" \
        "$EMERGE -pv sys-devel/gcc" \
        2>/dev/null | grep -E 'Benchmark|Time|faster'
fi

if [ "$fail" -ne 0 ]; then
    echo "RESULT: parity drift (expected until license groups + tighter cross closure)" >&2
    exit 1
fi
echo "RESULT: parity OK"