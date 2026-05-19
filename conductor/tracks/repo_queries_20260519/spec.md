# Spec: Repo Query Commands and Regen

## Overview

Implement the read-only repository query subcommands and the metadata cache
regeneration command, using `portage-repo` (md5-cache + EbuildShell) and
`portage-atom-pubgrub` (reverse dep lookup).  All commands operate on a
Gentoo repository tree specified by `--repo` (default `/var/db/repos/gentoo`).

No VDB access is required.  Each subcommand ships as one commit.

---

## Phase 1 — Read-only repo queries (md5-cache)

These commands open the repository, walk ebuilds, and read pre-generated
md5-cache entries.  No solver or shell is needed.

### `em query list [pattern]`

List packages in the repository.  Without a pattern print all CPVs one per
line.  With a pattern filter by glob or substring match against the CPV string
(`dev-libs/*`, `*ssl*`, exact `dev-libs/openssl`).

Output: one CPV per line, sorted.

### `em query which <atom>`

Print the absolute path to the best-matching ebuild for the given atom.
"Best" = highest version satisfying the version constraint (if any), or
latest version if no constraint.

Output: one path per line.

### `em query keywords <atom>`

Print a keyword table for all versions of the package.  Columns are
architectures, rows are versions.  Mark stable (`+`), testing (`~`),
disabled (`-`), missing (` `).

Output: human-readable table to stdout.

### `em query uses <atom>`

Print the IUSE flags for the best-matching version.  Show `+flag` for
enabled-by-default, `-flag` for disabled-by-default, `flag` for unset.

Output: space-separated list (like portage's output).

### `em search <pattern> [--description]`

Search package names (CPNs) for the pattern.  With `--description` also
search DESCRIPTION fields in the cache.

Output: `category/name — description` one per line, sorted by CPN.

### `em query hasuse <flag>`

List all packages (CPNs) whose IUSE includes the given flag.

Output: one CPN per line, sorted.

---

## Phase 2 — Solver-powered reverse deps

### `em query depends <atom>`

List packages whose RDEPEND or DEPEND contains an atom matching the given
dep.  Uses the portage-atom-pubgrub provider to load the full dep graph and
filter for reverse edges.

Output: one CPN per line, sorted.

---

## Phase 3 — Metadata cache regeneration

### `em regen [repos...]`

Regenerate the md5-cache for the specified repository (default: `--repo`
path).  Wraps the same EbuildShell-based pipeline as `regen_only.rs`.

Flags:
- `-j N` — parallel workers (default 20)
- `--dedup` — deduplicate dep entries (default on)
- `--output PATH` — write cache to a custom path instead of
  `<repo>/metadata/md5-cache`

Output: progress to stderr (`Total: N  Errors: M`), nothing to stdout.

---

## Acceptance Criteria

- Each command compiles and produces correct output on the frozen
  `/home/lu_zero/Sources/portage-repo/gentoo` tree
- No `unwrap()` in user-facing paths
- One commit per subcommand
- Errors reported via stderr + non-zero exit; stdout stays clean

## Out of Scope

- VDB queries (`query belongs`, `query files`, etc.)
- `emerge` execution / build phases
- metadata.xml parsing (`query meta`)
