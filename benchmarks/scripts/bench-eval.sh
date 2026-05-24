#!/usr/bin/env bash
# bench-eval.sh — evaluate benchmark sweep results and produce reports.
#
# Usage:
#   bench-eval.sh [options] [summary.tsv]
#
# If summary.tsv is omitted, uses the most recent bench-results/*/summary.tsv.
#
# Options:
#   -o, --output FILE     write to file instead of stdout
#   --filter TYPE         only include rows of type: criterion, regen, search
#   --filter-name PAT     only include rows where bench_name matches PAT
#   -h, --help            show this help
#
# Input format (summary.tsv from bench-sweep.sh):
#   type    config    bench_group    bench_name    metric    value    unit

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$(readlink -f "$0" 2>/dev/null || echo "$0")")" && pwd)
PROJ_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

FILTER_TYPE=""
FILTER_NAME=""
OUTPUT_FILE=""
INPUT=""

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --filter)       FILTER_TYPE="$2"; shift 2 ;;
        --filter-name)  FILTER_NAME="$2"; shift 2 ;;
        -o|--output)    OUTPUT_FILE="$2"; shift 2 ;;
        -h|--help)      usage ;;
        -*)             echo "unknown option: $1" >&2; exit 1 ;;
        *)              INPUT="$1"; shift ;;
    esac
done

if [[ -z "$INPUT" ]]; then
    latest=$(ls -td "$PROJ_ROOT"/bench-results/*/summary.tsv 2>/dev/null | head -1)
    if [[ -z "$latest" ]]; then
        echo "no bench-results found. Run bench-sweep.sh first." >&2
        exit 1
    fi
    INPUT="$latest"
    echo "using: $INPUT" >&2
fi

if [[ ! -f "$INPUT" ]]; then
    echo "file not found: $INPUT" >&2
    exit 1
fi

META=""
meta_file="$(dirname "$INPUT")/meta.env"
if [[ ! -f "$meta_file" ]]; then
    meta_file="$(dirname "$(dirname "$INPUT")")/meta.env"
fi
if [[ -f "$meta_file" ]]; then META="$meta_file"; fi

TMPDATA=$(mktemp)
trap 'rm -f "$TMPDATA"' EXIT

awk -F'\t' -v ft="$FILTER_TYPE" -v fn="$FILTER_NAME" '
    NR == 1 { next }
    ft != "" && $1 != ft { next }
    fn != "" && $4 !~ fn { next }
    { print }
' "$INPUT" > "$TMPDATA"

if [[ ! -s "$TMPDATA" ]]; then
    echo "no data to evaluate" >&2
    exit 0
fi

format_ns() {
    awk -v ns="$1" 'BEGIN {
        if (ns < 1000)        printf "%.1f ns", ns
        else if (ns < 1e6)    printf "%.2f \302\265s", ns/1e3
        else if (ns < 1e9)    printf "%.2f ms", ns/1e6
        else                  printf "%.3f s", ns/1e9
    }'
}

normalize_to_ns() {
    awk -v val="$1" -v unit="$2" 'BEGIN {
        ns = val + 0
        if (unit == "\302\265s") ns = val * 1000
        else if (unit == "ms") ns = val * 1e6
        else if (unit == "s")  ns = val * 1e9
        print ns
    }'
}

report_criterion() {
    echo ""
    echo "## Criterion Benchmarks"
    echo ""

    local groups
    groups=$(awk -F'\t' '$1 == "criterion" && $5 == "median" { print $3 }' "$TMPDATA" | sort -u)

    while IFS= read -r group; do
        [[ -z "$group" ]] && continue
        echo "### $group"
        echo ""

        local names
        names=$(awk -F'\t' -v g="$group" '$1 == "criterion" && $3 == g && $5 == "median" { print $4 }' "$TMPDATA" | sort -u)

        local configs
        configs=$(awk -F'\t' '$1 == "criterion" && $5 == "median" { print $2 }' "$TMPDATA" | sort -u)
        local c_arr=()
        while IFS= read -r c; do
            [[ -n "$c" ]] && c_arr+=("$c")
        done <<< "$configs"

        printf "| %-55s |" "benchmark"
        for c in "${c_arr[@]}"; do
            printf " %-18s |" "$c"
        done
        printf " %-10s |\n" "winner"
        printf "|%s|" "$(printf '%0.s-' {1..57})"
        for _ in "${c_arr[@]}"; do
            printf "%s|" "$(printf '%0.s-' {1..20})"
        done
        printf "%s|\n" "$(printf '%0.s-' {1..12})"

        while IFS= read -r bname; do
            [[ -z "$bname" ]] && continue
            printf "| %-55s |" "$bname"

            local min_ns="" winner=""
            local vals=()

            for c in "${c_arr[@]}"; do
                local row
                row=$(awk -F'\t' -v g="$group" -v n="$bname" -v cfg="$c" \
                    '$1 == "criterion" && $3 == g && $4 == n && $2 == cfg && $5 == "median" { print $6, $7; exit }' "$TMPDATA")
                local val="" unit="" ns_val=""
                if [[ -n "$row" ]]; then
                    read -r val unit <<< "$row"
                    ns_val=$(normalize_to_ns "$val" "$unit")
                fi
                vals+=("$ns_val")
                if [[ -n "$ns_val" ]]; then
                    if [[ -z "$min_ns" ]]; then
                        min_ns="$ns_val"
                    else
                        local smaller
                        smaller=$(awk -v a="$ns_val" -v b="$min_ns" 'BEGIN{print (a < b) ? 1 : 0}')
                        [[ "$smaller" == "1" ]] && min_ns="$ns_val"
                    fi
                fi
            done

            for i in "${!c_arr[@]}"; do
                local ns_val="${vals[$i]}"
                if [[ -z "$ns_val" ]]; then
                    printf " %-18s |" "---"
                else
                    local human
                    human=$(format_ns "$ns_val")
                    local is_min=0
                    if [[ -n "$min_ns" && -n "$ns_val" ]]; then
                        local eq
                        eq=$(awk -v a="$ns_val" -v b="$min_ns" 'BEGIN{print (a == b) ? 1 : 0}')
                        [[ "$eq" == "1" ]] && is_min=1
                    fi
                    if [[ "$is_min" -eq 1 ]]; then
                        printf " **%-14s** |" "$human"
                        winner="${c_arr[$i]}"
                    else
                        printf " %-18s |" "$human"
                    fi
                fi
            done
            printf " %-10s |\n" "$winner"
        done <<< "$names"
        echo ""
    done <<< "$groups"
}

report_regen() {
    echo "## Regen (wall-clock)"
    echo ""

    local rows
    rows=$(awk -F'\t' '$1 == "regen" && $5 == "real" { print $2, $6 }' "$TMPDATA")
    if [[ -z "$rows" ]]; then
        echo "_no regen data_"
        echo ""
        return
    fi

    printf "| %-25s | %-12s | %-12s | %-12s |\n" "config" "real" "user" "sys"
    printf "|%s|%s|%s|%s|\n" "$(printf '%0.s-' {1..27})" "$(printf '%0.s-' {1..14})" "$(printf '%0.s-' {1..14})" "$(printf '%0.s-' {1..14})"

    local configs
    configs=$(awk -F'\t' '$1 == "regen" && $5 == "real" { print $2 }' "$TMPDATA")
    while IFS= read -r config; do
        [[ -z "$config" ]] && continue
        local real user sys
        real=$(awk -F'\t' -v c="$config" '$1 == "regen" && $2 == c && $5 == "real" { print $6; exit }' "$TMPDATA")
        user=$(awk -F'\t' -v c="$config" '$1 == "regen" && $2 == c && $5 == "user" { print $6; exit }' "$TMPDATA")
        sys=$(awk -F'\t' -v c="$config" '$1 == "regen" && $2 == c && $5 == "sys" { print $6; exit }' "$TMPDATA")
        printf "| %-25s | %-12s | %-12s | %-12s |\n" "$config" "${real:-?}s" "${user:-?}s" "${sys:-?}s"
    done <<< "$configs"
    echo ""
}

report_search() {
    echo "## Search (wall-clock)"
    echo ""

    local rows
    rows=$(awk -F'\t' '$1 == "search" && $5 == "mean" { print $2, $4, $6 }' "$TMPDATA")
    if [[ -z "$rows" ]]; then
        echo "_no search data_"
        echo ""
        return
    fi

    printf "| %-25s | %-15s | %-10s | %-10s |\n" "config" "pattern" "mean" "min"
    printf "|%s|%s|%s|%s|\n" "$(printf '%0.s-' {1..27})" "$(printf '%0.s-' {1..17})" "$(printf '%0.s-' {1..12})" "$(printf '%0.s-' {1..12})"

    local entries
    entries=$(awk -F'\t' '$1 == "search" && $5 == "mean" { printf "%s\t%s\n", $2, $4 }' "$TMPDATA")
    while IFS=$'\t' read -r config pat; do
        [[ -z "$config" ]] && continue
        local mean min
        mean=$(awk -F'\t' -v c="$config" -v p="$pat" '$1 == "search" && $2 == c && $4 == p && $5 == "mean" { print $6; exit }' "$TMPDATA")
        min=$(awk -F'\t' -v c="$config" -v p="$pat" '$1 == "search" && $2 == c && $4 == p && $5 == "min" { print $6; exit }' "$TMPDATA")
        printf "| %-25s | %-15s | %-10s | %-10s |\n" "$config" "$pat" "${mean:-?}s" "${min:-?}s"
    done <<< "$entries"
    echo ""
}

result=""
{
    if [[ -n "$META" ]]; then
        timestamp=$(awk -F= '/^timestamp=/{print $2}' "$META")
        cpu=$(awk -F= '/^cpu=/{print $2}' "$META")
        cores=$(awk -F= '/^cores=/{print $2}' "$META")
        rustc=$(awk -F= '/^rustc=/{print $2}' "$META")
        configs=$(awk 'sub(/^configs=/, "")' "$META")
        repo=$(awk 'sub(/^repo=/, "")' "$META")
        regen_jobs=$(awk -F= '/^regen_jobs=/{print $2}' "$META")
    fi
    echo "# Benchmark Report"
    echo ""
    echo "| | |"
    echo "|---|---|"
    echo "| **Date** | ${timestamp:-unknown} |"
    echo "| **CPU** | ${cpu:-unknown} |"
    echo "| **Cores** | ${cores:-unknown} |"
    echo "| **rustc** | ${rustc:-unknown} |"
    echo "| **Configs** | ${configs:-unknown} |"
    echo "| **Repo** | ${repo:-unknown} |"
    echo "| **Regen jobs** | ${regen_jobs:-unknown} |"

    report_criterion
    report_regen
    report_search
} | {
    if [[ -n "$OUTPUT_FILE" ]]; then
        cat > "$OUTPUT_FILE"
        echo "written to $OUTPUT_FILE" >&2
    else
        cat
    fi
}
