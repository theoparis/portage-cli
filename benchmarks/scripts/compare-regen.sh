#!/usr/bin/env bash
# compare-regen.sh — wall-clock comparison of metadata-cache regeneration
# tools against the same Gentoo tree.
#
# Tools:
#   em regen -j N           — our Rust implementation (portage-cli)
#   pk repo metadata regen  — pkgcraft's implementation (Rust)
#
# egencache support (opt-in via INCLUDE_EGENCACHE=1) uses the *stock*
# egencache (no portage hacks; we stopped that). For each eg run it does:
#   sudo rm -rf "$REPO/metadata/md5-cache"
#   time sudo egencache -j $jobs --repo "$REPO_NAME" --update
# This is the "correct" way to get full cold exhaustive data points (the slow
# ones, ~4m37s real at j=20 on thalia per user measurement). It repopulates
# the live $REPO metadata as a side effect. The script reports the timing and
# the final file count from the live cache. em and pk use isolated -o/-p dirs
# and do not touch live caches.
#
# Usage: compare-regen.sh [jobs...]   (default: 24)
#   GENTOO_REPO=<path>     repo to regen (default: /var/db/repos/gentoo)
#   EM=<path>              em binary (default: search PATH then <ws>/target/release/em)
#   PK=<path>              pkgcraft pk binary (default: PATH then sibling ../pkgcraft/...)
#   EGENCACHE=<path>       egencache binary (default: command -v egencache; stock only)
#   ITERATIONS=N           runs per tool (default: 1)
#   SKIP=tool,tool         comma-separated list of tools to skip
#                          (em | egencache | pk)
#   INCLUDE_EGENCACHE=1    include egencache (uses sudo rm + sudo egencache --update
#                          on the live $REPO for the correct full cold slow datapoints)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO="${GENTOO_REPO:-/var/db/repos/gentoo}"
ITERATIONS="${ITERATIONS:-1}"
SKIP="${SKIP:-}"
INCLUDE_EGENCACHE="${INCLUDE_EGENCACHE:-0}"

# Locate egencache (stock, no patches). We use the system one by default
# so we do not rely on any hacked portage tree. You can override EGENCACHE=...
# to point at a specific binary (e.g. a local build of portage's egencache).
EGENCACHE="${EGENCACHE:-}"
if [[ -z "$EGENCACHE" ]]; then
    EGENCACHE=$(command -v egencache 2>/dev/null || true)
fi

# egencache is driven by repo *name* (from the tree's profiles/repo_name, usually "gentoo")
# plus the location that sudo/root sees for that name (or via PORTAGE_REPOSITORIES).
REPO_NAME=""
if [[ -f "$REPO/profiles/repo_name" ]]; then
    REPO_NAME=$(< "$REPO/profiles/repo_name" | tr -d '[:space:]')
fi

# For reproducible single-NUMA-node runs on large multisocket/NUMA machines
# (e.g. thalia 4-node Ampere), bind to node 0. This avoids cross-NUMA latency
# in eclass sourcing / metadata phases. Set NUMACTL="" to disable.
if [[ -z "${NUMACTL:-}" ]]; then
    if command -v numactl >/dev/null 2>&1; then
        if numactl --cpunodebind=0 --membind=0 true 2>/dev/null; then
            NUMACTL="numactl --cpunodebind=0 --membind=0"
        else
            NUMACTL=""
        fi
    else
        NUMACTL=""
    fi
fi

if [[ $# -gt 0 ]]; then JOBS=("$@"); else JOBS=(24); fi

# Locate binaries. Fallbacks are relative to this script's location so it
# works when invoked from any cwd (e.g. as benchmarks/scripts/compare-regen.sh).
EM="${EM:-}"
if [[ -z "$EM" ]]; then
    if command -v em >/dev/null 2>&1; then
        EM=$(command -v em)
    elif [[ -x "$SCRIPT_DIR/../../target/release/em" ]]; then
        EM=$(realpath "$SCRIPT_DIR/../../target/release/em")
    fi
fi

PK="${PK:-}"
if [[ -z "$PK" ]]; then
    if command -v pk >/dev/null 2>&1; then
        PK=$(command -v pk)
    else
        for cand in \
            "$SCRIPT_DIR/../../../pkgcraft/target/release/pk" \
            "$SCRIPT_DIR/../../pkgcraft/target/release/pk" \
            "$SCRIPT_DIR/../pkgcraft/target/release/pk" \
            ; do
            if [[ -x "$cand" ]]; then PK=$(realpath "$cand"); break; fi
        done
    fi
fi

skip_tool() { [[ ",$SKIP," == *",$1,"* ]]; }

declare -a TOOLS=()
declare -A TOOL_DESC=()
if [[ -n "$EM" && -x "$EM" ]] && ! skip_tool em; then
    TOOLS+=(em); TOOL_DESC[em]="$EM"
fi
if [[ "$INCLUDE_EGENCACHE" == "1" && -n "$EGENCACHE" && -n "$REPO_NAME" ]] \
        && ! skip_tool egencache; then
    TOOLS+=(egencache); TOOL_DESC[egencache]="$EGENCACHE (stock via sudo rm + sudo egencache --update for correct full cold data points)"
fi
if [[ -n "$PK" && -x "$PK" ]] && ! skip_tool pk; then
    TOOLS+=(pk); TOOL_DESC[pk]="$PK"
fi

if [[ ${#TOOLS[@]} -eq 0 ]]; then
    echo "no tools found to compare" >&2; exit 1
fi

echo "Repository: $REPO"
echo "Iterations per tool: $ITERATIONS"
echo "Jobs sweep: ${JOBS[*]}"
echo "Tools:"
for t in "${TOOLS[@]}"; do echo "  $t  ->  ${TOOL_DESC[$t]}"; done
echo

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Run a tool. $1=tool, $2=jobs, $3=out_dir, $4=log_path.
# Times the actual command with `time`; prints "real user sys" on stdout.
run_one() {
    local tool="$1" jobs="$2" out_dir="$3" log="$4"
    local tf
    tf=$(mktemp)

    case "$tool" in
        em)
            # Current CLI syntax: positional repo path (not --repo).
            { time $NUMACTL "$EM" regen "$REPO" -o "$out_dir" -j "$jobs" \
                >"$log" 2>&1; } 2>"$tf"
            ;;
        egencache)
            # The correct/full-cold way for egencache (the one that produces the
            # expected full results and the slow timings the user measured):
            # explicitly sudo rm the source md5-cache under the repo, then
            # sudo egencache --update (stock, no --cache-dir because we stopped
            # hacking portage). This forces a true cold exhaustive run over every
            # ebuild. It is slow (minutes) but gives the real data point.
            # Side effect: it repopulates the live $REPO/metadata/md5-cache.
            # We still create $out_dir and drop a marker file so the files= report
            # works (count taken from the live cache after the run).
            sudo rm -rf "$REPO/metadata/md5-cache" || true
            local eg_cmd
            if [[ -n "$NUMACTL" ]]; then
                eg_cmd=(sudo $NUMACTL "$EGENCACHE" --repo "$REPO_NAME" --jobs="$jobs" --update)
            else
                eg_cmd=(sudo "$EGENCACHE" --repo "$REPO_NAME" --jobs="$jobs" --update)
            fi
            { time "${eg_cmd[@]}" >"$log" 2>&1 ; } 2>"$tf" || true
            mkdir -p "$out_dir"
            local live_cnt
            live_cnt=$(find "$REPO/metadata/md5-cache" -type f 2>/dev/null | wc -l || echo 0)
            echo "$live_cnt" > "$out_dir/.eg_files_count"
            ;;
        pk)
            { time $NUMACTL "$PK" repo metadata regen -j "$jobs" -p "$out_dir" -f -n "$REPO" \
                >"$log" 2>&1; } 2>"$tf"
            ;;
        *)
            echo "unknown tool: $tool" >&2; rm -f "$tf"; return 1
            ;;
    esac

    awk '/real|user|sys/{printf "%s ", $2} END{print ""}' "$tf"
    rm -f "$tf"
}

printf "%-12s  %-4s  %-4s  %-12s  %-12s  %-12s\n" \
    "tool" "j" "run" "real" "user" "sys"

for J in "${JOBS[@]}"; do
    for tool in "${TOOLS[@]}"; do
        for ((i = 1; i <= ITERATIONS; i++)); do
            out_dir="$WORK/$tool-j$J-i$i"
            log="$WORK/$tool-j$J-i$i.log"
            mkdir -p "$out_dir"
            read -r real user sys < <(run_one "$tool" "$J" "$out_dir" "$log")
            printf "%-12s  %-4s  %-4s  %-12s  %-12s  %-12s\n" \
                "$tool" "$J" "$i" "$real" "$user" "$sys"
            if [[ "$tool" == "egencache" ]]; then
                cnt=$(cat "$out_dir/.eg_files_count" 2>/dev/null || echo 0)
            else
                cnt=$(find "$out_dir" -type f | wc -l)
            fi
            echo "    files=$cnt"
        done
    done
done
