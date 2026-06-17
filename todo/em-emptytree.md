# `--emptytree` (`-e`) — design and implementation

Tracking emerge parity for `em -pe` / `em -e`. Updated 2026-06-17 after
rejecting the `grok-broke-emptytree` approach (`prepend_host_build_pretend`,
`trim_bootstrap_gcc`, skip-`host_installed` hacks).

## What emerge does

User-facing semantics (`emerge -h`):

> Reinstalls target atoms and their entire deep dependency tree, **as though no
> packages are currently installed**.

Implementation is **not** “delete the VDB”. It is a **selection policy** on the
merge target (`ROOT`), with separate rules for the build host (`BROOT`).

`create_depgraph_params` with `--emptytree`:

- `empty = True` — do not **select** installed packages as merge candidates
- `deep = True` — traverse the full dependency tree
- `selective` removed
- `bdeps = auto` by default (unless `--with-bdeps` overrides)

Core depgraph rule (`depgraph.py` ~7707):

```python
if empty and pkg.installed and not excluded:
    continue  # do not select installed packages
```

| Axis | `--emptytree` on native `/` |
|------|-----------------------------|
| **Selection** | Installed TARGET packages never chosen as merge candidates |
| **VDB read** | Still read — action tags (`N`/`R`/`U`), slot logic, display |
| **BDEPEND** | Satisfied against **BROOT** (host `/var/db/pkg`) — the stage1 seed |
| **Deep closure** | Same-version deps appear as `[ebuild R]`, not omitted |

## Not the same as `--root <empty>`

| Mode | Target VDB | Flag | Purpose |
|------|------------|------|---------|
| Stage / offset | Literally empty | *(none)* | `em -p --root <empty> @system` |
| Native reinstall | Physically full | `--emptytree` | `emerge -pe firefox` on `/` |

Commit `43f7cf6` fixed the stage case (split target vs sysroot installed
views). That is orthogonal to `--emptytree`.

## The “*stage1 bdeps” tension

“Nothing installed” applies to **ROOT** (runtime / merge target). **BROOT**
(host `/`) is always assumed to carry the bootstrap toolchain:

- `BDEPEND` / `IDEPEND` → satisfied on BROOT
- Host `sys-devel/gcc` satisfies `BDEPEND` of target `gcc` → breaks the cycle
- Without this, you cannot bootstrap gcc from literal nothing on one arch

PMS dep-class roots (see `docs/root-model.md`):

| Class | Satisfied against |
|-------|-------------------|
| `RDEPEND` | `ROOT` |
| `DEPEND` | `SYSROOT` / `ESYSROOT` |
| `BDEPEND` / `IDEPEND` | `BROOT` (always host `/`) |

## What went wrong before

### v1 (`3e051d1`)

Skipped `add_installed` entirely when `empty` — treated emptytree as “solver
sees no installed packages”. Wrong: portage still reads the VDB.

### v2 (`installed.rs` + current master)

`load_target_installed(roots, empty)` returned `Vec::new()` — broke action
tags (everything `N`), post-solve filtering (dropped same-version `R` lines),
and hid build-time dep expansion.

### v3 (`grok-broke-emptytree` — reverted)

Compensating hacks on top of v2:

1. `load_tag_installed` — split view for tags only (good idea, wrong foundation)
2. Skip `host_installed` during solve
3. Auto `with_bdeps`
4. `prepend_host_build_pretend` — post-solve fixpoint to fake host `R` lines
5. `trim_bootstrap_gcc` — ad-hoc gcc slot trimming

Symptom: `firefox --emptytree` 411 vs emerge 400 CPVs; `bash --emptytree`
missing `pkgconf`, all tags `N` instead of `R`/`U`.

## Clean model (three layers)

```
TARGET (ROOT)     selection: --emptytree ⇒ never pick installed CPVs for merge
                  satisfaction: RDEPEND/DEPEND checked against target VDB
                  display: action tags still compare against real VDB

BROOT (host /)    satisfaction: BDEPEND/IDEPEND checked against host VDB
                  stage1 assumption: gcc, cmake, perl, … already present
```

No post-solve pretend layer. The solver and post-solve passes implement the
split directly.

## Implementation plan

### 1. `InstalledPolicy::Rebuild` (`portage-atom-pubgrub`) — **done**

New policy for packages registered from the target VDB under `--emptytree`:

- `choose_version`: never return the installed version (fall through to newest
  repo candidate, like “not favored”)
- `get_dependencies`: even when selected version == installed version, return
  **full** build-time deps (not runtime-only shortcut)

### 2. `rebuild_tree` flag on `PortageDependencyProvider` — **done**

When true (native `--emptytree`):

- Skip virtual/OR “prefer installed branch” heuristics in `choose_version`
- `get_dependencies`: still uses `broot_filter` during the solve (Tier C expand
  re-adds host-satisfied build tools afterward)
- Set from depgraph: `empty && !host_config_stage && !cross.active`

### 2b. Tier C — `emptytree_expand.rs` — **done**

Post-solve fixpoint (`expand_satisfied_rebuilds`):

1. Walk **all five** PMS dep fields from every **real ebuild** in the plan
   (`BDEPEND`, `IDEPEND`, `DEPEND`, `RDEPEND`, `PDEPEND`) — category-agnostic
2. Skip only [`PortagePackage::is_virtual()`] solver-internal nodes (Choice /
   UseDecision — no md5-cache metadata). This is **not** the same as `virtual/*`
   ebuilds, which are walked like any other package
3. Re-add atoms missing from the plan but already satisfied on the correct root:
   BROOT for `BDEPEND`/`IDEPEND`, ROOT for `DEPEND`/`RDEPEND`/`PDEPEND`
4. Pick best accepted repo version; trim superseded toolchain slots

#### Expand pass pitfalls (read before editing)

| Mistake | Symptom | Correct model |
|---------|---------|---------------|
| Limit `RDEPEND`/`PDEPEND` to `virtual/*` or `app-alternatives/*` | `firefox -pe` missing perl tail (`List-MoreUtils-XS`, `File-ShareDir`, …) | Emerge recurses satisfied edges for **any** parent when `deep` is active |
| Confuse `PortagePackage::is_virtual()` with `virtual/*` ebuilds | `virtual/pkgconfig` never expands to `pkgconf` | Category `virtual/*` = real ebuild; `is_virtual()` = solver node |
| Only walk `BDEPEND`/`IDEPEND` | `po4a` → `opensp` chain missing | Also walk `DEPEND` (target-satisfied build deps) |
| Zero the target VDB under `-e` | All tags `N`, no `R` lines | Emptytree is a **selection** policy; VDB stays loaded |
| `prepend_host_build_pretend` without host gate | `rust-bin` slot fan-out | Re-add only when `Avail::atom_satisfied` on the matching root |

Regression anchors:

```bash
emerge -pe app-shells/bash   # 6 CPVs, pkgconf in closure
em -pe app-shells/bash

emerge -pe www-client/firefox
em -pe www-client/firefox
# Expect ~396/400 shared CPVs; List-MoreUtils-XS present in both
```

Reference chain that caught the RDEPEND-only-virtual bug:

```
Syntax-Keyword-Try → BDEPEND → XS-Parse-Keyword → RDEPEND → File-ShareDir
  → List-MoreUtils → PDEPEND xs? → List-MoreUtils-XS
```

### 3. `portage-cli` depgraph (`mod.rs`) — **done**

- **Stop clearing** `target_installed` on `empty` — always load real VDB
- Register target packages with `InstalledPolicy::Rebuild` when `emptytree_native`
- Pass **empty** `installed_cpvs` to the repo `Adapter` under emptytree (so
  `cede_required_use` does not skip packages being rebuilt)
- Keep **full** `target_installed_cpvs` for action tags and tree display
- Post-solve order filter: when `emptytree_native`, **keep** same-version CPVs
  in the plan (do not drop “already installed” entries)
- `solve_with_bdeps = with_bdeps || emptytree_native` (emerge `bdeps=auto`)
- `host_installed`: always wired — no skip

### 4. Builder (`main.rs`)

Unchanged: `--emptytree` disables VDB-resume skip during merge. Planner and
builder concerns stay separate.

## Verification

```bash
# Action tags and closure
emerge -pe app-shells/bash
em -pe app-shells/bash
# Expect: R on deps, U on bash, pkgconf/pkgconfig present

# Stage path unaffected
em -p --root <empty> --config-root / @system
# Expect: ~181 target CPVs (with --with-bdeps=n parity from 43f7cf6)

# Normal -p unchanged
em -p app-shells/bash
# Expect: single U line (no regression)
```

## Correctness audit (2026-06-17)

### Small / medium closures — **exact**

| Case | emerge | em | Non-toolchain diff |
|------|--------|-----|-------------------|
| `bash -pe` | 6 | 6 | 0 |
| `zlib -pe` | 1 | 1 | 0 |
| `firefox -p` (no `-e`) | 79 | 79 | 0 |

Action tags on `bash -pe` match (`R`/`U`, same old-version brackets).

### Large closures — **userspace ~99% CPV match, small toolchain gap**

`firefox -pe` on this host (Tier C + unified dep walk, 2026-06-17):

| Metric | Value |
|--------|-------|
| emerge CPVs | 400 |
| em CPVs | 401 |
| **Shared (identical CPV)** | **396 (99%)** |
| emerge-only | 4 |
| em-only | 5 |

emerge-only (toolchain / one lib): `gcc`, `binutils`, `grep`, `simdjson`.

em-only (stale bootstrap slots): `rust-1.93.1`, `autoconf`, `python-3.12`,
`libmd`, `libbsd`.

### Toolchain divergence (the entire gap)

**emerge-only** (missing from em):

- `dev-lang/rust-{1.94.0,1.95.0}` + `rust-common-1.95.0` (source rust)
- `dev-libs/oniguruma-6.9.10`
- `llvm-22` clang/compiler-rt stack (partial)
- `sys-devel/{gcc-16,binutils-2.46.1}` upgrades

**em-only** (spurious in em):

- Six `dev-lang/rust-bin-*` slots (1.74 … 1.94) — fan-out instead of one path
- Full `llvm-16` bootstrap stack (clang/llvm/compiler-rt)
- `sys-devel/gcc-11.5.0` + `binutils-2.44-r4` (old bootstrap slots)
- Old `autoconf-{2.13,2.69}` / `automake-1.17` / `python-3.12` (NS)

Host already has `gcc-16`, current `binutils`, and source `dev-lang/rust`.
Emerge upgrades the host toolchain (`U gcc-16`, `U binutils`) and builds rust
from source. em schedules parallel **bootstrap slots** (gcc-11, llvm-16,
multiple rust-bin) because `rebuild_tree` disables `broot_filter` entirely —
every BDEPEND edge in the deep closure is expanded onto ROOT, and the solver
does not collapse “host already has a newer toolchain in another slot.”

**Correctness verdict:** for `-pe`, the **userspace / perl / doc build closure
is effectively emerge-identical** after the unified dep-class expand. The
remaining gap is **toolchain upgrade scheduling** (emerge upgrades host
`gcc`/`binutils`; em keeps host versions) plus a few em-only bootstrap slots.

**Likely fix direction** (not implemented): smarter toolchain slot collapse
and host-upgrade selection under `rebuild_tree` (without reverting
`broot_filter` during solve or bringing back `prepend_host_build_pretend`).

## Open / deferred

- Host VDB version for same-slot `R` lines (not always newest repo)
- Toolchain upgrade parity (gcc/binutils/rust/llvm paths)
- `--exclude` under emptytree (portage allows excluded installed pkgs as
  providers; `em` parses `-X` but does not wire it)
- `--emptytree` on cross / host-config-stage combinations