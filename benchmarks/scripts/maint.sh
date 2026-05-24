#!/usr/bin/env bash
# maint.sh — workspace maintenance for the portage-* / gentoo-* Rust crates.
#
# Modes:
#   setup    [DIR]   git-clone every workspace repo into DIR
#   update   [DIR]   git pull --ff-only in every repo found in DIR
#   patch    [DIR]   insert a [patch.crates-io] block in every consumer crate
#                    so sibling deps resolve against local paths
#   unpatch  [DIR]   strip [patch.crates-io] from every consumer crate
#                    (use before tagging releases — verifies that the
#                     registry version of each dep is actually reachable)
#   status   [DIR]   show each repo's branch / ahead-behind / dirty state
#
# DIR defaults to the parent of the script's own directory — i.e. the
# workspace root, with portage-bench (where this script lives) as one of
# its siblings. Pass DIR explicitly to override.
#
# Repos are siblings under DIR. The script assumes that layout for the
# relative paths it writes into [patch.crates-io].

set -euo pipefail

# Script lives in <workspace>/portage-bench/scripts/. Default DIR is the
# workspace root (the parent of portage-bench), so clones land as siblings
# of portage-bench. Override by passing DIR explicitly.
SCRIPT_DIR=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
DEFAULT_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)

# Each entry: subdir|git-url|crate-name
# crate-name matches subdir for all repos now.
REPOS=(
    "gentoo-interner|git@github.com:lu-zero/gentoo-interner|gentoo-interner"
    "gentoo-core|git@github.com:lu-zero/gentoo-core|gentoo-core"
    "gentoo-stages|git@github.com:lu-zero/gentoo-stages|gentoo-stages"
    "portage-atom|git@github.com:lu-zero/portage-atom|portage-atom"
    "portage-atom-pubgrub|git@github.com:lu-zero/portage-atom-pubgrub|portage-atom-pubgrub"
    "portage-atom-resolvo|git@github.com:lu-zero/portage-atom-resolvo|portage-atom-resolvo"
    "portage-metadata|git@github.com:lu-zero/portage-metadata|portage-metadata"
    "portage-repo|git@github.com:lu-zero/portage-repo|portage-repo"
    "portage-bench|git@github.com:lu-zero/portage-bench|portage-bench"
    "portage-cli|git@github.com:lu-zero/portage-cli|portage-cli"
)

# Consumers that should carry a [patch.crates-io] block during local dev.
# (Crates that depend on the registry version of any other workspace crate.)
CONSUMERS=(
    gentoo-core
    gentoo-stages
    portage-atom
    portage-atom-pubgrub
    portage-atom-resolvo
    portage-metadata
    portage-repo
    portage-bench
    portage-cli
)

# Patchable crates we emit local-path overrides for. Order is the canonical
# release order; the same order appears in the generated [patch.crates-io].
#
# Only crates that are (or will be) published to crates.io belong here.
# portage-repo and portage-cli are intentionally omitted — they are pulled
# in via path-only deps everywhere they are used, so a registry override
# never resolves to anything and cargo would just warn about an unused
# patch entry.
PATCHABLE_SUBDIRS=(
    gentoo-interner
    gentoo-core
    portage-atom
    portage-atom-pubgrub
    portage-atom-resolvo
    portage-metadata
)

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
}

# Resolve a target dir to an absolute path, creating it if it doesn't exist.
resolve_dir() {
    local d="${1:-$DEFAULT_ROOT}"
    mkdir -p "$d"
    (cd "$d" && pwd)
}

# Look up the crate-name for a subdir from REPOS.
crate_of() {
    local subdir="$1"
    for entry in "${REPOS[@]}"; do
        local s="${entry%%|*}"
        if [ "$s" = "$subdir" ]; then
            echo "$entry" | cut -d'|' -f3
            return 0
        fi
    done
    return 1
}

cmd_setup() {
    local root
    root="$(resolve_dir "${1:-$DEFAULT_ROOT}")"
    echo "setup -> $root"
    for entry in "${REPOS[@]}"; do
        local subdir="${entry%%|*}"
        local url
        url=$(echo "$entry" | cut -d'|' -f2)
        local target="$root/$subdir"
        if [ -d "$target/.git" ]; then
            printf '  %-22s already cloned\n' "$subdir"
            continue
        fi
        printf '  %-22s cloning from %s\n' "$subdir" "$url"
        git clone "$url" "$target"
    done
}

cmd_update() {
    local root
    root="$(resolve_dir "${1:-$DEFAULT_ROOT}")"
    echo "update -> $root"

    # Unpatch first so git pull doesn't conflict with Cargo.toml changes.
    local had_patch=()
    for subdir in "${CONSUMERS[@]}"; do
        local toml="$root/$subdir/Cargo.toml"
        if [ -f "$toml" ] && grep -q '^\[patch\.crates-io\]' "$toml"; then
            strip_patch_block "$toml"
            had_patch+=("$subdir")
        fi
    done

    for entry in "${REPOS[@]}"; do
        local subdir="${entry%%|*}"
        local target="$root/$subdir"
        if [ ! -d "$target/.git" ]; then
            printf '  %-22s not cloned (run setup)\n' "$subdir"
            continue
        fi
        local out
        out=$(cd "$target" && git pull --ff-only 2>&1 | tr '\n' ' ' | sed 's/ *$//')
        printf '  %-22s %s\n' "$subdir" "$out"
    done

    # Re-apply patches that were present before the update.
    if [ ${#had_patch[@]} -gt 0 ]; then
        echo ""
        echo "  re-patching:"
        for subdir in "${had_patch[@]}"; do
            local toml="$root/$subdir/Cargo.toml"
            write_patch_block "$toml" "$root" "$subdir"
            printf '    %-22s patched\n' "$subdir"
        done
    fi
}

cmd_status() {
    local root
    root="$(resolve_dir "${1:-$DEFAULT_ROOT}")"
    echo "status -> $root"
    for entry in "${REPOS[@]}"; do
        local subdir="${entry%%|*}"
        local target="$root/$subdir"
        if [ ! -d "$target/.git" ]; then
            printf '  %-22s (missing)\n' "$subdir"
            continue
        fi
        local branch ahead behind dirty patched
        branch=$(cd "$target" && git symbolic-ref --short HEAD 2>/dev/null || echo "DETACHED")
        ahead=$(cd "$target" && git rev-list --count "@{u}..HEAD" 2>/dev/null || echo "?")
        behind=$(cd "$target" && git rev-list --count "HEAD..@{u}" 2>/dev/null || echo "?")
        dirty=$(cd "$target" && git status --porcelain 2>/dev/null | grep -cv '^??' || true)
        patched=no
        if [ -f "$target/Cargo.toml" ] && grep -q '^\[patch\.crates-io\]' "$target/Cargo.toml"; then
            patched=yes
        fi
        printf '  %-22s branch=%-10s ahead=%-3s behind=%-3s dirty=%-2s patched=%s\n' \
            "$subdir" "$branch" "$ahead" "$behind" "$dirty" "$patched"
    done
}

# Drop any existing [patch.crates-io] section, plus trailing blank lines, in place.
strip_patch_block() {
    local file="$1"
    local tmp="${file}.maint.tmp"
    # Awk: print everything outside a [patch.crates-io] section. The section
    # runs from its header to the next `[section]` line (exclusive) or EOF.
    awk '
        /^\[patch\.crates-io\]/ { in_patch=1; next }
        in_patch && /^\[/       { in_patch=0 }
        !in_patch               { print }
    ' "$file" > "$tmp"
    # Trim trailing blank lines.
    awk 'BEGIN{blank=0} /^$/{blank++; next} {while (blank-- > 0) print ""; blank=0; print} END{}' "$tmp" > "$file"
    rm -f "$tmp"
}

write_patch_block() {
    local file="$1"
    local root="$2"
    local self="$3"   # subdir of the crate being patched — skip self-patch
    local cr crate
    {
        echo ""
        echo "[patch.crates-io]"
        for cr in "${PATCHABLE_SUBDIRS[@]}"; do
            if [ "$cr" = "$self" ]; then
                continue
            fi
            if [ -d "$root/$cr" ]; then
                crate=$(crate_of "$cr")
                echo "$crate = { path = \"../$cr\" }"
            fi
        done
    } >> "$file"
}

cmd_patch() {
    local root
    root="$(resolve_dir "${1:-$DEFAULT_ROOT}")"
    echo "patch -> $root"
    for subdir in "${CONSUMERS[@]}"; do
        local toml="$root/$subdir/Cargo.toml"
        if [ ! -f "$toml" ]; then
            printf '  %-22s (missing — skip)\n' "$subdir"
            continue
        fi
        strip_patch_block "$toml"
        write_patch_block "$toml" "$root" "$subdir"
        printf '  %-22s patched\n' "$subdir"
    done
}

cmd_unpatch() {
    local root
    root="$(resolve_dir "${1:-$DEFAULT_ROOT}")"
    echo "unpatch -> $root"
    for subdir in "${CONSUMERS[@]}"; do
        local toml="$root/$subdir/Cargo.toml"
        if [ ! -f "$toml" ]; then
            printf '  %-22s (missing — skip)\n' "$subdir"
            continue
        fi
        strip_patch_block "$toml"
        printf '  %-22s unpatched\n' "$subdir"
    done
}

cmd="${1:-help}"
shift || true

case "$cmd" in
    setup)   cmd_setup   "${1:-$DEFAULT_ROOT}" ;;
    update)  cmd_update  "${1:-$DEFAULT_ROOT}" ;;
    patch)   cmd_patch   "${1:-$DEFAULT_ROOT}" ;;
    unpatch) cmd_unpatch "${1:-$DEFAULT_ROOT}" ;;
    status)  cmd_status  "${1:-$DEFAULT_ROOT}" ;;
    help|--help|-h) usage ;;
    *) echo "unknown command: $cmd" >&2; usage; exit 1 ;;
esac
