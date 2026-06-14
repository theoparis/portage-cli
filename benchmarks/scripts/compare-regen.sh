#!/usr/bin/env bash
# compare-regen.sh — wall-clock comparison of metadata-cache regeneration
# tools against the same Gentoo tree.
#
# Tools:
#   em regen -j N           — our Rust implementation (portage-cli)
#   pk repo metadata regen  — pkgcraft's implementation (Rust)
#   egencache --update      — Portage's official tool (Python)
#                             [opt-in via INCLUDE_EGENCACHE=1]
#
# em and pk both accept a custom output directory (-o / -p) so they
# write into isolated tempdirs.  egencache is invoked via the patched
# portage-3.0.79 source (if present) using --cache-dir + --external-cache-only
# so it also writes to an isolated flat dir (no live repo pollution).
# INCLUDE_EGENCACHE=1 to include it.
#
# Usage: compare-regen.sh [jobs...]   (default: 24)
#   GENTOO_REPO=<path>     repo to regen (default: /var/db/repos/gentoo)
#   EM=<path>              em binary (default: search PATH then <ws>/target/release/em)
#   EGENCACHE=<path>       egencache (default: PATH or /home/lu_zero/Sources/portage-3.0.79/bin/egencache)
#   PK=<path>              pkgcraft pk binary (default: PATH then sibling ../pkgcraft/...)
#   ITERATIONS=N           runs per tool (default: 1)
#   SKIP=tool,tool         comma-separated list of tools to skip
#                          (em | egencache | pk)
#   INCLUDE_EGENCACHE=1    include egencache in the comparison (uses --cache-dir for isolation)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO="${GENTOO_REPO:-/var/db/repos/gentoo}"
ITERATIONS="${ITERATIONS:-1}"
SKIP="${SKIP:-}"
INCLUDE_EGENCACHE="${INCLUDE_EGENCACHE:-0}"

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

# Prefer the patched source egencache from the portage-3.0.79 tree if present
# (for the --cache-dir direct output support added for benchmark isolation).
EGENCACHE="${EGENCACHE:-}"
if [[ -z "$EGENCACHE" ]]; then
    if [[ -x "/home/lu_zero/Sources/portage-3.0.79/bin/egencache" ]]; then
        EGENCACHE="/home/lu_zero/Sources/portage-3.0.79/bin/egencache"
    else
        EGENCACHE=$(command -v egencache 2>/dev/null || true)
    fi
fi

# egencache wants the repo *name*, plus a config snippet pointing at the path.
REPO_NAME=""
if [[ -f "$REPO/profiles/repo_name" ]]; then
    REPO_NAME=$(< "$REPO/profiles/repo_name")
fi

skip_tool() { [[ ",$SKIP," == *",$1,"* ]]; }

declare -a TOOLS=()
declare -A TOOL_DESC=()
if [[ -n "$EM" && -x "$EM" ]] && ! skip_tool em; then
    TOOLS+=(em); TOOL_DESC[em]="$EM"
fi
if [[ "$INCLUDE_EGENCACHE" == "1" && -n "$EGENCACHE" && -n "$REPO_NAME" ]] \
        && ! skip_tool egencache; then
    TOOLS+=(egencache); TOOL_DESC[egencache]="$EGENCACHE (using --cache-dir + --external-cache-only for full-blown isolated flat cache)"
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
            { time "$EM" regen "$REPO" -o "$out_dir" -j "$jobs" \
                >"$log" 2>&1; } 2>"$tf"
            ;;
        egencache)
            # Use patched egencache --cache-dir + --external-cache-only to write
            # a full scan directly into isolated $out_dir (flat cpv files).
            # To get apples-to-apples with em/pk (exhaustive full tree, no
            # KEYWORDS/profile filtering), we create a temp --config-root that
            # forces ACCEPT_KEYWORDS="**" and (if available) a broad amd64
            # profile from the tree. This makes egencache consider every ebuild.
            mkdir -p "$out_dir"
            eg_config_root=$(mktemp -d)
            mkdir -p "$eg_config_root"/portage
            echo 'ACCEPT_KEYWORDS="**"' > "$eg_config_root"/portage/make.conf
            # Try to use a broad profile from the tree (amd64 one typically has
            # wide support); ** should make all visible anyway.
            profile_dir=$(find "$REPO/profiles" -path '*default/linux/amd64*' -type d 2>/dev/null | head -1 || true)
            if [[ -n "$profile_dir" ]]; then
                ln -s "$profile_dir" "$eg_config_root"/portage/make.profile 2>/dev/null || echo "profile = $profile_dir" > "$eg_config_root"/portage/make.profile
            fi
            egencache_cmd=("$EGENCACHE" --config-root "$eg_config_root")
            egencache_cmd=(env ACCEPT_KEYWORDS="**" "PORTAGE_REPOSITORIES=gentoo=$REPO" "${egencache_cmd[@]}")
            { time "${egencache_cmd[@]}" --update --repo "$REPO_NAME" \
                --jobs="$jobs" \
                --cache-dir "$out_dir" --external-cache-only \
                >"$log" 2>&1; } 2>"$tf" || true
            rm -rf "$eg_config_root"
            ;;
        pk)
            { time "$PK" repo metadata regen -j "$jobs" -p "$out_dir" -f -n "$REPO" \
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
            cnt=$(find "$out_dir" -type f | wc -l)
            echo "    files=$cnt"
        done
    done
done
