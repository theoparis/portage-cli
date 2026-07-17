# Short-flag parity with real `emerge`

2026-07-17: quick survey of `man emerge`'s short options against `em`'s own
`Cli`/`MergeFlags`/`DepgraphFlags` struct source (more reliable than a
possibly-truncated `--help` dump). Landed the two obvious gaps; the rest are
noted below, not started.

## Landed

- **`-C`/`--unmerge`** — remove installed packages directly, no dependency
  checking at all (distinct from `depclean`'s automatic orphan cleanup).
  `emerge.rs::unmerge_atoms`, shares its removal core with the existing
  in-place-replace path via `ebuild::unmerge_package`.
- **`-B`/`--buildpkgonly`** — build a binary package straight from the image,
  never merge/install it. Computes CONTENTS/metadata by walking the image and
  registering into a scratch VDB dir (`ebuild::build_binpkg_standalone`),
  matching real portage's own `EbuildBuild.py` model (never calls `merge()`
  for `-B` either). Live/root/VDB genuinely untouched, verified in the
  crossdev-stages sandbox.

Committed as two separate commits: `-C` in `a32c217`, `-B` in `d5d4eb5`
(both also fixed by a Fable review pass to respect `--ask`, which the
first draft missed for `-C`).

## Remaining gaps (not started)

- **`-c`/`--depclean`** — the big one. Not a flag addition; needs the
  reverse-dependency/orphan-detection machinery `depclean` requires (world
  file + reverse deps of the full installed set). Real feature work, not
  "low hanging" like `-C`/`-B` were.
- **`-P`/`--prune`** — companion to `--depclean` (remove all but the best
  version of a match, ignoring deps entirely). Depends on `-c` existing
  first, or could stand alone — not investigated.
- **`-r`/`--resume`** — replay the last aborted/skipped merge list. Needs
  `em` to persist a resumable plan somewhere between invocations; no such
  state exists today.
- **`-U`/`--changed-use`** — like `-N`/`--newuse` but only rebuilds on a USE
  *change* relative to what's installed, not "profile default changed
  underneath you". `em` has `-N`; check whether the distinction is worth a
  separate flag or whether `-N`'s existing logic already covers this case.
- **`-W`/`--deselect`** — remove atoms from the world file without
  unmerging. `em`'s own `-w` short flag is already taken (`em ebuild`'s
  `--work-dir` override), so this would need a different short form or
  long-only, same reasoning as `--tree` losing its short form to `--target`.
- **`-F`/`--fetch-all-uri`** — minor, fetch every SRC_URI including
  unused-by-current-USE ones. Low priority.

None of these were asked for beyond the `-C`/`-B` pair; listed here so a
future "what's next" survey doesn't have to re-derive the man-page diff from
scratch.
