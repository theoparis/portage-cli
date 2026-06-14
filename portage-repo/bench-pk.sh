#!/usr/bin/env bash
# bench-pk.sh — benchmark pk metadata regen at multiple thread counts
# Usage: bench-pk.sh [jobs...]   (default: 4 8 16 20 24 32 40)
#   GENTOO_REPO=<path>  override repo path
#   PK=<path>           override pk binary path

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="${GENTOO_REPO:-/var/db/repos/gentoo}"

# Find pk robustly: honor PK=, or look relative to common layouts
# (pkgcraft sibling to portage-cli/, or to portage-repo/)
if [[ -z "${PK:-}" ]]; then
    for cand in \
        "$SCRIPT_DIR/../../pkgcraft/target/release/pk" \
        "$SCRIPT_DIR/../pkgcraft/target/release/pk" \
        "$SCRIPT_DIR/../../../pkgcraft/target/release/pk" \
        ; do
        if [[ -x "$cand" ]]; then PK="$cand"; break; fi
    done
fi
PK="${PK:-}"

if [[ ! -x "$PK" ]]; then
    echo "pk binary not found: $PK (set PK=... or place in ../pkgcraft etc)" >&2
    exit 1
fi

if [[ $# -gt 0 ]]; then JOBS=("$@"); else JOBS=(4 8 16 20 24 32 40); fi

tree_rss() {
    local pid=$1 total=0 rss child
    rss=$(awk '/VmRSS/{print $2}' /proc/"$pid"/status 2>/dev/null || echo 0)
    total=$((total + rss))
    for child in $(pgrep -P "$pid" 2>/dev/null); do
        rss=$(tree_rss "$child")
        total=$((total + rss))
    done
    echo "$total"
}

peak_rss() {
    local pid=$1 max=0 rss
    while kill -0 "$pid" 2>/dev/null; do
        rss=$(tree_rss "$pid")
        [[ $rss -gt $max ]] && max=$rss
        sleep 0.05
    done
    echo "$max"
}

printf "%-4s  %-10s  %-10s  %-10s  %s\n" "j" "real" "user" "sys" "peak RSS"
for J in "${JOBS[@]}"; do
    OUT=$(mktemp -d)
    { time "$PK" repo metadata regen -j "$J" -p "$OUT" -n -f "$REPO" >/dev/null 2>&1; } 2>/tmp/bench_time &
    BGPID=$!
    RSS=$(peak_rss "$BGPID")
    wait "$BGPID"
    REAL=$(awk '/real/{print $2}' /tmp/bench_time)
    USER=$(awk '/user/{print $2}' /tmp/bench_time)
    SYS=$(awk '/sys/{print $2}'  /tmp/bench_time)
    printf "%-4s  %-10s  %-10s  %-10s  %d MB\n" "$J" "$REAL" "$USER" "$SYS" "$((RSS / 1024))"
    rm -rf "$OUT"
done
