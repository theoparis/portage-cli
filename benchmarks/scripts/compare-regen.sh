#!/usr/bin/env bash
# compare-regen.sh — wall-clock comparison of metadata-cache regeneration
# tools against the same Gentoo tree.
#
# Tools:
#   em regen -j N           — our Rust implementation (portage-cli)
#   pk repo metadata regen  — pkgcraft's implementation (Rust)
#
# egencache support (opt-in via INCLUDE_EGENCACHE=1; default off) uses stock
# egencache (no portage source hacks). When enabled, runs the *exact* plain
# command the user specified for the "correct" full cold results (no NUMACTL,
# no extra):
#   sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache
#   sudo egencache -j $jobs --repo gentoo --update
# (hardcoded live gentoo; guarantees the slow full datapoints like 4m37s at j=20).
# em/pk use GENTOO_REPO (set to /var/db/repos/gentoo for same-tree compare).
# Repo name defaults to "gentoo" (won't silently skip).
# Output is a valid markdown table.
# Set INCLUDE_EGENCACHE=1 to include; EGENCACHE=... if not in PATH.
#
# Usage: compare-regen.sh [jobs...]   (default: 24)
#   GENTOO_REPO=<path>     repo to regen (default: /var/db/repos/gentoo)
#   EM=<path>              em binary (default: search PATH then <ws>/target/release/em)
#   PK=<path>              pkgcraft pk binary (default: PATH then sibling ../pkgcraft/...)
#   EGENCACHE=<path>       egencache binary (default: command -v egencache; stock only)
#   ITERATIONS=N           runs per tool (default: 1)
#   SKIP=tool,tool         comma-separated list of tools to skip
#                          (em | egencache | pk)
#   INCLUDE_EGENCACHE=1    set to 1 to include egencache (default off; runs the plain
#                          sudo rm + sudo egencache -j N --repo gentoo --update on live)

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
if [[ -z "$REPO_NAME" ]]; then
    REPO_NAME=gentoo
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
            # Exact plain command the user specified for the "correct" full results:
            #   sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache
            #   sudo egencache -j $jobs --repo gentoo --update
            # (hardcoded to live gentoo; no NUMACTL, no extra args/env. This collects
            # the slow full datapoints. em/pk use GENTOO_REPO; set to /var/db for same tree.)
            local EG_LIVE="/var/db/repos/gentoo"
            local EG_RNAME="gentoo"
            sudo rm -rf "$EG_LIVE/metadata/md5-cache" || true
            local eg_cmd=( sudo "$EGENCACHE" -j "$jobs" --repo "$EG_RNAME" --update )
            { time "${eg_cmd[@]}" >"$log" 2>&1 ; } 2>"$tf" || true
            mkdir -p "$out_dir"
            local live_cnt
            live_cnt=$(find "$EG_LIVE/metadata/md5-cache" -type f 2>/dev/null | wc -l || echo 0)
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

echo '| tool | j | run | real | user | sys | files |'
echo '|------|---|-----|------|------|-----|-------|'

for J in "${JOBS[@]}"; do
    for tool in "${TOOLS[@]}"; do
        for ((i = 1; i <= ITERATIONS; i++)); do
            out_dir="$WORK/$tool-j$J-i$i"
            log="$WORK/$tool-j$J-i$i.log"
            mkdir -p "$out_dir"
            read -r real user sys < <(run_one "$tool" "$J" "$out_dir" "$log")
            if [[ "$tool" == "egencache" ]]; then
                cnt=$(cat "$out_dir/.eg_files_count" 2>/dev/null || echo 0)
            else
                cnt=$(find "$out_dir" -type f | wc -l)
            fi
            printf '| %-12s | %-4s | %-4s | %-12s | %-12s | %-12s | %s |\n' \
                "$tool" "$J" "$i" "$real" "$user" "$sys" "$cnt"
        done
    done
done
