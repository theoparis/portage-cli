#!/bin/bash
# Measure post-solve BDEPEND within-run trim: wall time and plan size.
#
# Usage: benchmarks/bench-bdepend-trim.sh [path-to-em]
#   EM=target/release/em
#   CROSS_CHOST=riscv64-unknown-linux-gnu
#   SYSROOT=/usr/${CROSS_CHOST}
#   ARCH=riscv64
#   ACCEPT_LICENSE=*
#   RUNS=3

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
    echo "error: $EM not found" >&2
    exit 1
fi

count_ebuilds() {
    grep -c '^\[ebuild' || true
}

em_cmd() {
    ACCEPT_LICENSE="$ACCEPT_LICENSE" \
        "$EM" -p --config-root "$SYSROOT" --root "$SYSROOT" --arch "$ARCH" "$@"
}

echo "== plan size (cross sys-devel/gcc)"
for mode in default with-bdeps; do
    args=(sys-devel/gcc)
    [ "$mode" = with-bdeps ] && args=(--with-bdeps sys-devel/gcc)
    n=$(em_cmd "${args[@]}" 2>/dev/null | count_ebuilds)
    emerge_n=""
    if command -v "$EMERGE" >/dev/null 2>&1; then
        if [ "$mode" = with-bdeps ]; then
            emerge_n=$("$EMERGE" -pv --with-bdeps=y sys-devel/gcc 2>/dev/null | count_ebuilds)
        else
            emerge_n=$("$EMERGE" -pv sys-devel/gcc 2>/dev/null | count_ebuilds)
        fi
    fi
    printf '   em %-12s %3s' "$mode" "$n"
    [ -n "$emerge_n" ] && printf '  emerge %3s' "$emerge_n"
    echo
done

echo
echo "== wall time (hyperfine, $RUNS runs, trim is always on in this build)"
if command -v hyperfine >/dev/null 2>&1; then
    hyperfine -w 1 -r "$RUNS" --ignore-failure \
        "ACCEPT_LICENSE='$ACCEPT_LICENSE' $EM -p --config-root $SYSROOT --root $SYSROOT --arch $ARCH sys-devel/gcc" \
        "ACCEPT_LICENSE='$ACCEPT_LICENSE' $EM -p --config-root $SYSROOT --root $SYSROOT --arch $ARCH --with-bdeps sys-devel/gcc" \
        2>/dev/null | grep -E 'Benchmark|Time|faster'
else
    echo "   (hyperfine not installed — skip timing)"
fi