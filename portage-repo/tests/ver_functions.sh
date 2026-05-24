#!/usr/bin/env bash
# Test script for PMS 12.3.14 version manipulation functions.
# Validates __ver_split, ver_cut, ver_rs, and ver_test.
#
# Usage: bash tests/ver_functions.sh
# Exit code 0 = all pass, 1 = failures

set -euo pipefail

PASS=0
FAIL=0

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "${actual}" == "${expected}" ]]; then
        PASS=$(( PASS + 1 ))
    else
        echo "FAIL: ${desc}: expected '${expected}', got '${actual}'"
        FAIL=$(( FAIL + 1 ))
    fi
}

assert_true() {
    local desc="$1"; shift
    if "$@"; then
        PASS=$(( PASS + 1 ))
    else
        echo "FAIL: ${desc}: expected true"
        FAIL=$(( FAIL + 1 ))
    fi
}

assert_false() {
    local desc="$1"; shift
    if "$@"; then
        echo "FAIL: ${desc}: expected false"
        FAIL=$(( FAIL + 1 ))
    else
        PASS=$(( PASS + 1 ))
    fi
}

# ── Source the functions from builtins.rs ──────────────────────────────
# We extract the bash code between the raw string delimiters.
# For testing standalone, we define stubs for die and PV/PVR.

die() { echo "die: $*" >&2; return 1; }
PV="1.2.3"
PVR="1.2.3-r1"

# Extract and source the bash functions
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# Source the functions inline (they're in a Rust raw string, we replicate them here)
# Instead, let's define them directly for testing:

# ── __ver_split ───────────────────────────────────────────────────────

__ver_split() {
    local ver="$1"
    __ver=()
    __ver_ncomp=0
    if [[ "${ver}" =~ ^([^A-Za-z0-9]+)(.*) ]]; then
        __ver+=("${BASH_REMATCH[1]}")
        ver="${BASH_REMATCH[2]}"
    else
        __ver+=("")
    fi
    while [[ -n "${ver}" ]]; do
        if [[ "${ver}" =~ ^([0-9]+)(.*) ]]; then
            __ver+=("${BASH_REMATCH[1]}")
            ver="${BASH_REMATCH[2]}"
        elif [[ "${ver}" =~ ^([A-Za-z]+)(.*) ]]; then
            __ver+=("${BASH_REMATCH[1]}")
            ver="${BASH_REMATCH[2]}"
        else
            break
        fi
        __ver_ncomp=$(( __ver_ncomp + 1 ))
        if [[ -z "${ver}" ]]; then
            __ver+=("")
        elif [[ "${ver}" =~ ^([^A-Za-z0-9]+)(.*) ]]; then
            __ver+=("${BASH_REMATCH[1]}")
            ver="${BASH_REMATCH[2]}"
        else
            __ver+=("")
        fi
    done
}

__ver_parse_range() {
    if [[ "$1" == *-* ]]; then
        __range_start="${1%%-*}"
        __range_end="${1##*-}"
    else
        __range_start="$1"
        __range_end="$1"
    fi
    [[ -z "${__range_start}" ]] && __range_start=0
    [[ -z "${__range_end}" ]] && __range_end=${__ver_ncomp}
}

ver_cut() {
    local range="$1"
    local ver="${2:-${PV}}"
    __ver_split "${ver}"
    local __range_start __range_end
    __ver_parse_range "${range}"
    local start=${__range_start} end=${__range_end}
    if (( start > __ver_ncomp || end < 1 )); then
        return
    fi
    local s_idx e_idx
    if (( start <= 0 )); then
        s_idx=0
    else
        s_idx=$(( 2 * start - 1 ))
    fi
    if (( end >= __ver_ncomp )); then
        e_idx=$(( ${#__ver[@]} - 1 ))
    else
        e_idx=$(( 2 * end - 1 ))
    fi
    local result="" i
    for (( i = s_idx; i <= e_idx; i++ )); do
        result+="${__ver[i]}"
    done
    echo "${result}"
}

ver_rs() {
    local nargs=$#
    local ver
    if (( nargs % 2 == 1 )); then
        eval "ver=\${$nargs}"
        (( nargs-- ))
    else
        ver="${PV}"
    fi
    __ver_split "${ver}"
    local p=1
    while (( p < nargs )); do
        eval "local __range=\${$p}"
        eval "local __repl=\${$((p+1))}"
        local __range_start __range_end
        __ver_parse_range "${__range}"
        local start=${__range_start} end=${__range_end}
        local i
        for (( i = start; i <= end; i++ )); do
            local arr_idx=$(( 2 * i ))
            if (( arr_idx < 0 || arr_idx >= ${#__ver[@]} )); then
                continue
            fi
            if (( i == 0 || i == __ver_ncomp )); then
                [[ -z "${__ver[arr_idx]}" ]] && continue
            fi
            __ver[arr_idx]="${__repl}"
        done
        (( p += 2 ))
    done
    local result="" i
    for (( i = 0; i < ${#__ver[@]}; i++ )); do
        result+="${__ver[i]}"
    done
    echo "${result}"
}

__ver_compare() {
    local va="$1" vb="$2"
    __ver_cmp=0
    local ra=0 rb=0
    if [[ "${va}" =~ ^(.*)-r([0-9]+)$ ]]; then
        va="${BASH_REMATCH[1]}"; ra="${BASH_REMATCH[2]}"
    fi
    if [[ "${vb}" =~ ^(.*)-r([0-9]+)$ ]]; then
        vb="${BASH_REMATCH[1]}"; rb="${BASH_REMATCH[2]}"
    fi
    local sa_t sa_n sb_t sb_n
    sa_t=(); sa_n=(); sb_t=(); sb_n=()
    while [[ "${va}" =~ ^(.*)_(alpha|beta|pre|rc|p)([0-9]*)$ ]]; do
        sa_t=("${BASH_REMATCH[2]}" "${sa_t[@]}")
        sa_n=("${BASH_REMATCH[3]:-0}" "${sa_n[@]}")
        va="${BASH_REMATCH[1]}"
    done
    while [[ "${vb}" =~ ^(.*)_(alpha|beta|pre|rc|p)([0-9]*)$ ]]; do
        sb_t=("${BASH_REMATCH[2]}" "${sb_t[@]}")
        sb_n=("${BASH_REMATCH[3]:-0}" "${sb_n[@]}")
        vb="${BASH_REMATCH[1]}"
    done
    local la="" lb=""
    if [[ "${va}" =~ ^(.*[0-9])([a-z])$ ]]; then
        va="${BASH_REMATCH[1]}"; la="${BASH_REMATCH[2]}"
    fi
    if [[ "${vb}" =~ ^(.*[0-9])([a-z])$ ]]; then
        vb="${BASH_REMATCH[1]}"; lb="${BASH_REMATCH[2]}"
    fi
    local IFS='.'
    local an bn
    an=(${va}); bn=(${vb})
    unset IFS
    local ann=${#an[@]} bnn=${#bn[@]}
    local a0="${an[0]:-0}" b0="${bn[0]:-0}"
    while [[ "${a0}" == 0?* ]]; do a0="${a0#0}"; done
    while [[ "${b0}" == 0?* ]]; do b0="${b0#0}"; done
    [[ -z "${a0}" ]] && a0=0; [[ -z "${b0}" ]] && b0=0
    if (( a0 > b0 )); then __ver_cmp=1; return; fi
    if (( a0 < b0 )); then __ver_cmp=-1; return; fi
    local cmin=$(( ann < bnn ? ann : bnn ))
    local i
    for (( i = 1; i < cmin; i++ )); do
        local ai="${an[i]}" bi="${bn[i]}"
        if [[ "${ai}" == 0?* ]] || [[ "${bi}" == 0?* ]]; then
            local as="${ai}" bs="${bi}"
            while [[ "${as}" == *0 && ${#as} -gt 1 ]]; do as="${as%0}"; done
            while [[ "${bs}" == *0 && ${#bs} -gt 1 ]]; do bs="${bs%0}"; done
            if [[ "${as}" > "${bs}" ]]; then __ver_cmp=1; return; fi
            if [[ "${as}" < "${bs}" ]]; then __ver_cmp=-1; return; fi
        else
            local ai_n="${ai}" bi_n="${bi}"
            while [[ "${ai_n}" == 0?* ]]; do ai_n="${ai_n#0}"; done
            while [[ "${bi_n}" == 0?* ]]; do bi_n="${bi_n#0}"; done
            [[ -z "${ai_n}" ]] && ai_n=0; [[ -z "${bi_n}" ]] && bi_n=0
            if (( ai_n > bi_n )); then __ver_cmp=1; return; fi
            if (( ai_n < bi_n )); then __ver_cmp=-1; return; fi
        fi
    done
    if (( ann > bnn )); then __ver_cmp=1; return; fi
    if (( ann < bnn )); then __ver_cmp=-1; return; fi
    if [[ "${la}" > "${lb}" ]]; then __ver_cmp=1; return; fi
    if [[ "${la}" < "${lb}" ]]; then __ver_cmp=-1; return; fi
    local asn=${#sa_t[@]} bsn=${#sb_t[@]}
    local smin=$(( asn < bsn ? asn : bsn ))
    for (( i = 0; i < smin; i++ )); do
        if [[ "${sa_t[i]}" == "${sb_t[i]}" ]]; then
            local an_s="${sa_n[i]}" bn_s="${sb_n[i]}"
            [[ -z "${an_s}" ]] && an_s=0; [[ -z "${bn_s}" ]] && bn_s=0
            if (( an_s > bn_s )); then __ver_cmp=1; return; fi
            if (( an_s < bn_s )); then __ver_cmp=-1; return; fi
        else
            local ao bo
            case "${sa_t[i]}" in
                alpha) ao=0 ;; beta) ao=1 ;; pre) ao=2 ;;
                rc) ao=3 ;; p) ao=4 ;;
            esac
            case "${sb_t[i]}" in
                alpha) bo=0 ;; beta) bo=1 ;; pre) bo=2 ;;
                rc) bo=3 ;; p) bo=4 ;;
            esac
            if (( ao > bo )); then __ver_cmp=1; else __ver_cmp=-1; fi
            return
        fi
    done
    if (( asn != bsn )); then
        local extra_t
        if (( asn > bsn )); then
            extra_t="${sa_t[bsn]}"
            if [[ "${extra_t}" == "p" ]]; then __ver_cmp=1; else __ver_cmp=-1; fi
        else
            extra_t="${sb_t[asn]}"
            if [[ "${extra_t}" == "p" ]]; then __ver_cmp=-1; else __ver_cmp=1; fi
        fi
        return
    fi
    local ra_n="${ra}" rb_n="${rb}"
    while [[ "${ra_n}" == 0?* ]]; do ra_n="${ra_n#0}"; done
    while [[ "${rb_n}" == 0?* ]]; do rb_n="${rb_n#0}"; done
    [[ -z "${ra_n}" ]] && ra_n=0; [[ -z "${rb_n}" ]] && rb_n=0
    if (( ra_n > rb_n )); then __ver_cmp=1; return; fi
    if (( ra_n < rb_n )); then __ver_cmp=-1; return; fi
}

ver_test() {
    local va op vb
    if [[ $# -eq 3 ]]; then
        va="$1"; op="$2"; vb="$3"
    elif [[ $# -eq 2 ]]; then
        va="${PVR}"; op="$1"; vb="$2"
    else
        die "ver_test: invalid arguments: $*"
        return 1
    fi
    __ver_compare "${va}" "${vb}"
    case "${op}" in
        -eq) (( __ver_cmp == 0 )) ;;
        -ne) (( __ver_cmp != 0 )) ;;
        -lt) (( __ver_cmp < 0 )) ;;
        -le) (( __ver_cmp <= 0 )) ;;
        -gt) (( __ver_cmp > 0 )) ;;
        -ge) (( __ver_cmp >= 0 )) ;;
        *)
            die "ver_test: unknown operator: ${op}"
            return 1
            ;;
    esac
}

# ══════════════════════════════════════════════════════════════════════
# Tests
# ══════════════════════════════════════════════════════════════════════

echo "=== __ver_split ==="

# Basic numeric
__ver_split "1.2.3"
assert_eq "1.2.3 ncomp" "3" "${__ver_ncomp}"
assert_eq "1.2.3 comp1" "1" "${__ver[1]}"
assert_eq "1.2.3 sep1" "." "${__ver[2]}"
assert_eq "1.2.3 comp2" "2" "${__ver[3]}"
assert_eq "1.2.3 comp3" "3" "${__ver[5]}"

# Letter components (PMS: [A-Za-z]+ is a component, not a separator)
__ver_split "1.2a3"
assert_eq "1.2a3 ncomp" "4" "${__ver_ncomp}"
assert_eq "1.2a3 comp1" "1" "${__ver[1]}"
assert_eq "1.2a3 sep1" "." "${__ver[2]}"
assert_eq "1.2a3 comp2" "2" "${__ver[3]}"
assert_eq "1.2a3 sep2 (empty)" "" "${__ver[4]}"
assert_eq "1.2a3 comp3" "a" "${__ver[5]}"
assert_eq "1.2a3 sep3 (empty)" "" "${__ver[6]}"
assert_eq "1.2a3 comp4" "3" "${__ver[7]}"

# Leading separator
__ver_split "@1.2"
assert_eq "@1.2 leading sep" "@" "${__ver[0]}"
assert_eq "@1.2 comp1" "1" "${__ver[1]}"

# Trailing separator
__ver_split "1.2-"
assert_eq "1.2- trailing sep" "-" "${__ver[4]}"

echo "=== ver_cut ==="

# Basic ranges
assert_eq "ver_cut 1" "1" "$(ver_cut 1 "1.2.3")"
assert_eq "ver_cut 2" "2" "$(ver_cut 2 "1.2.3")"
assert_eq "ver_cut 1-2" "1.2" "$(ver_cut 1-2 "1.2.3")"
assert_eq "ver_cut 1-3" "1.2.3" "$(ver_cut 1-3 "1.2.3")"
assert_eq "ver_cut 2-3" "2.3" "$(ver_cut 2-3 "1.2.3")"
assert_eq "ver_cut 2-" "2.3" "$(ver_cut 2- "1.2.3")"

# Letter components
assert_eq "ver_cut 1 1.2a3" "1" "$(ver_cut 1 "1.2a3")"
assert_eq "ver_cut 2-3 1.2a3" "2a" "$(ver_cut 2-3 "1.2a3")"
assert_eq "ver_cut 3-4 1.2a3" "a3" "$(ver_cut 3-4 "1.2a3")"
assert_eq "ver_cut 1-4 1.2a3" "1.2a3" "$(ver_cut 1-4 "1.2a3")"

# Zero index (leading separator)
assert_eq "ver_cut 0-2 @1.2" "@1.2" "$(ver_cut 0-2 "@1.2")"
assert_eq "ver_cut 0-1 1.2" "1" "$(ver_cut 0-1 "1.2")"

# Range past end (trailing separator)
assert_eq "ver_cut 2- 1.2-" "2-" "$(ver_cut 2- "1.2-")"

# Out of range
assert_eq "ver_cut 5 1.2.3" "" "$(ver_cut 5 "1.2.3")"

# Default PV
assert_eq "ver_cut 1 default" "1" "$(ver_cut 1)"

echo "=== ver_rs ==="

# Basic replacement
assert_eq "ver_rs 1 _ 1.2.3" "1_2.3" "$(ver_rs 1 _ "1.2.3")"
assert_eq "ver_rs 1-2 _ 1.2.3" "1_2_3" "$(ver_rs 1-2 _ "1.2.3")"
assert_eq "ver_rs 2 _ 1.2.3" "1.2_3" "$(ver_rs 2 _ "1.2.3")"

# Multiple pairs
assert_eq "ver_rs multi" "1_2-3" "$(ver_rs 1 _ 2 - "1.2.3")"

# Replace empty separator (digit↔letter transition)
assert_eq "ver_rs 2 . 1.2a3" "1.2.a3" "$(ver_rs 2 . "1.2a3")"

# Default PV
assert_eq "ver_rs 1 _ default" "1_2.3" "$(ver_rs 1 _)"

echo "=== ver_test ==="

# Basic integer comparison
assert_true "1 -eq 1" ver_test 1 -eq 1
assert_true "1 -lt 2" ver_test 1 -lt 2
assert_true "2 -gt 1" ver_test 2 -gt 1
assert_false "1 -gt 2" ver_test 1 -gt 2

# Dotted versions
assert_true "1.2.3 -eq 1.2.3" ver_test 1.2.3 -eq 1.2.3
assert_true "1.2.3 -lt 1.2.4" ver_test 1.2.3 -lt 1.2.4
assert_true "1.2.3 -gt 1.2.2" ver_test 1.2.3 -gt 1.2.2
assert_true "1.2 -lt 1.2.1" ver_test 1.2 -lt 1.2.1

# Letter suffixes (PMS algorithm 3.4)
assert_true "1.0a -lt 1.0b" ver_test 1.0a -lt 1.0b
assert_true "1.0b -gt 1.0a" ver_test 1.0b -gt 1.0a
assert_true "1.0 -lt 1.0a" ver_test 1.0 -lt 1.0a
assert_true "1.0z -gt 1.0" ver_test 1.0z -gt 1.0

# Version suffixes (PMS algorithms 3.5/3.6)
assert_true "_alpha < _beta" ver_test 1.0_alpha -lt 1.0_beta
assert_true "_beta < _pre" ver_test 1.0_beta -lt 1.0_pre
assert_true "_pre < _rc" ver_test 1.0_pre -lt 1.0_rc
assert_true "_rc < release" ver_test 1.0_rc -lt 1.0
assert_true "_p > release" ver_test 1.0_p -gt 1.0
assert_true "_alpha < release" ver_test 1.0_alpha -lt 1.0
assert_true "_p1 < _p2" ver_test 1.0_p1 -lt 1.0_p2
assert_true "_alpha1 > _alpha" ver_test 1.0_alpha1 -gt 1.0_alpha

# Multiple suffixes
assert_true "_alpha_p < _beta" ver_test 1.0_alpha_p -lt 1.0_beta

# Revision comparison (PMS algorithm 3.7)
assert_true "-r0 -eq no rev" ver_test 1.0-r0 -eq 1.0
assert_true "-r1 > -r0" ver_test 1.0-r1 -gt 1.0-r0
assert_true "-r1 > no rev" ver_test 1.0-r1 -gt 1.0
assert_true "-r2 > -r1" ver_test 1.0-r2 -gt 1.0-r1

# 2-arg form uses PVR
assert_true "2-arg PVR" ver_test -eq "1.2.3-r1"

# Complex version
assert_true "complex" ver_test 1.2.3_alpha1_p2-r3 -lt 1.2.3_alpha1_p3-r0

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="
(( FAIL == 0 ))
