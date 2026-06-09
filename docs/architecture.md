# Architecture

The main architecture reference for this workspace. It describes **how the
pieces fit together and how `em` resolves a dependency plan**. For the
per-crate public-API catalog and crates.io/publishing status, see the
root [`ARCHITECTURE.md`](../ARCHITECTURE.md). For the deeper USE/solver
contract, see [`portage-atom-pubgrub/docs/use-and-solver-boundary.md`](../portage-atom-pubgrub/docs/use-and-solver-boundary.md);
for performance numbers and the running gap list, see the
[`portage-atom-pubgrub` README](../portage-atom-pubgrub/README.md).

> **Slop warning.** This codebase is largely AI-generated. Verify a claim
> against the code before relying on it; update this file when it drifts.

## Crate layering

Lower crates know nothing of higher ones. The edges below are the real
`Cargo.toml` dependencies.

```
gentoo-interner ─┐
                 ├─ gentoo-core ─────────────┐
portage-atom ────┤                           │
   │             ├─ portage-metadata ─┐       │
   │             │                    ├─ portage-repo ─┐
   ├─ portage-atom-pubgrub ───────────┼───────────────┼─ portage-cli (em)
   └─ portage-atom-resolvo ───────────┘  portage-distfiles, portage-vdb ┘
```

- **portage-atom** — PMS atom parsing (`Cpn`, `Cpv`, `Dep`, `Version`,
  `DepEntry`, USE-deps, slot operators). The vocabulary every other crate speaks.
- **portage-metadata** — ebuild metadata: md5-cache `CacheEntry`/`EbuildMetadata`,
  EAPI, keywords, `IUse`, and the `LicenseExpr` / `RequiredUseExpr` / `RestrictExpr`
  sub-grammars. Pure parsing + evaluation; no I/O.
- **portage-repo** — reads a repository from disk: ebuild discovery, profile
  stack resolution, manifests, and an embedded `bash` (`EbuildShell`, via brush)
  for sourcing `make.defaults`/`make.conf` and ebuilds.
- **portage-atom-pubgrub** / **portage-atom-resolvo** — two interchangeable
  solver bridges over the same facts. `em` uses the PubGrub bridge; resolvo is
  kept for cross-checking. Both consume a `PackageRepository` of *facts* and
  *resolved policy* and never resolve profile/`make.conf`/`package.use` themselves.
- **portage-cli** — the `em` binary: wires repo + profile + VDB into a solver,
  runs post-solve checks, and renders emerge-compatible output.

## The `em -p` / `em query depgraph` pipeline

`em -p` and `em query depgraph` share one path (`query/depgraph/mod.rs`).
Stages, in order:

1. **Load facts** (`repo.rs`) — parse the repo's md5-cache into `RepoData`
   (CPN → versions → `CacheEntry`), filtered by keywords/mask/license.
2. **Build the USE environment** (`use_env.rs` → `portage-repo`) — see
   [USE stacking](#use-stacking-precedence) below. Produces the global
   `UseConfig`, `package.use`, `USE_EXPAND` groups, masks, `ACCEPT_KEYWORDS`,
   `ACCEPT_LICENSE`.
3. **Load installed set** (`installed.rs`) — the VDB, used for `InstalledPolicy`
   (`Favor`/`Lock`), action tags (`N`/`R`/`U`/`D`), and reverse-dep checks.
4. **Build the provider** (`PortageDependencyProvider::new(adapter)`) — the cli
   `Adapter` implements `PackageRepository`, handing the solver each version's
   facts (`versions_for`) and its resolved **desired** USE (`desired_use`).
5. **Resolve** (`resolve_targets`) — PubGrub selects one version per package,
   modelling OR/`^^`/`??` groups, slots/subslots, USE-conditional deps, and
   USE-dep constraints (the latter via virtual `UseDecision` packages). When the
   post-solve pass decides to upgrade an installed package to a newer version
   (`upgrade_to`), `resolve_targets` pins that version and re-solves to a
   (bounded) fixpoint, so the upgraded version's full dependency closure is part
   of the plan — not an unsolved approximation; a re-solve that cannot be
   satisfied falls back to the last good solution.
6. **Post-solve checks** — see [Post-solve validation](#post-solve-validation).
7. **Install order** (`install_order`) — SCC condensation (iterative Tarjan) +
   lexicographic Kahn; hard (DEPEND/BDEPEND) edges before soft (RDEPEND); cycles
   broken on soft edges. Explicitly-requested targets are listed last when
   nothing depends on them (emerge convention).
8. **Render** (`output.rs`) — `pretty` (emerge `-p`/`-pv`), `json`, or `tree`.
   Verbose `-pv` also shows per-package download size and a "Size of downloads"
   total (`download_size.rs`): distfiles from each package's `Manifest`,
   restricted to what `SRC_URI` needs for the effective USE, minus those already
   in `DISTDIR`, deduplicated across the plan — matching `emerge -pv` exactly.
9. **Advisory warnings** — emitted *after* the plan (emerge lists issues at the
   bottom), see [Post-solve validation](#post-solve-validation).

## USE stacking precedence

This is the part most easily gotten wrong, so it is pinned here. `em` resolves a
package's effective USE in the same incremental order Portage does
(low → high precedence; later layers override earlier, `-flag` removes):

1. `make.globals`
2. profile `make.defaults` (stacked through the profile parents)
3. `make.conf`
4. **the `USE` environment variable** (and each `USE_EXPAND` key read from the
   process env, e.g. `PYTHON_TARGETS=...`)
5. `package.use` (profile + `/etc/portage/package.use`)
6. `use.force` / `use.mask`, and the per-package `package.use.force` /
   `package.use.mask` (plus the `*.stable.*` variants for stable-keyword merges)

Portage also appends **`/etc/portage/profile/`** as a *site-local profile layer*
on top of the resolved `make.profile` chain (portage(5),
`LocationsManager`'s `CUSTOM_PROFILE_PATH`) — a flat node whose own `parent` file
is not followed. `ProfileStack::with_user_profile` folds it in, so its
`make.defaults` (layer 2), `package.use` (layer 5) and `use.force`/`use.mask`
(layer 6) all take effect at the *highest* priority. Per PMS 5.2.4 any of these
profile files may be a *directory* whose regular files are concatenated in
filename order (`/etc/portage/profile/package.use.mask/<name>` is the common
case); `read_lines` handles both forms.

Layers 1–4, plus the **global** `use.force`/`use.mask`, are computed in
`portage-repo`'s `resolve_use_flags` (`build/profile.rs`): the profile chain and
`make.conf` are sourced through the embedded shell, then `apply_env_layer` merges
the `USE`/`USE_EXPAND` env vars, and finally global `use.force`/`use.mask` add/remove
flags. So `USE="-X" em -p www-client/firefox` *does* enter the stack — but
`package.use`/`use.force` (layers 5–6) sit above it and can pin a flag back on.

Layer 5 (`package.use`) is applied **per package** at solve/display time via
`apply_package_use`. The **per-package** parts of layer 6 — `package.use.force`/
`package.use.mask` and all `*.stable.*` variants — are applied on top of that by
`force_mask.rs` (`ForceMask`): force enables a flag, mask disables it (mask wins),
overriding `package.use` and the configured value, exactly as Portage does. The
`*.stable.*` sets apply only when the version is "merged due to a stable keyword"
(`force_mask::is_stable`, mirroring Portage's `KeywordsManager.isStable`: accepted
*and* `ACCEPT_KEYWORDS` does not accept `~arch`), so they are inert on a `~arch`
system. This is what makes crossdev's `cross-*` `multilib`/`cet`/`nopie` pins take
effect. Force/mask are applied in **both** consumers: `desired_use` (the solver's
view, so conditional deps fire correctly) and the display fold in `mod.rs` (which
appends synthetic `package.use` entries so output, the `REQUIRED_USE` check,
download-size and autounmask all agree). The solver itself never recomputes any of
this; it consumes the resolved `desired` set (see the
[USE/solver boundary doc](../portage-atom-pubgrub/docs/use-and-solver-boundary.md)).

## Post-solve validation

The solver decides *versions*; several constraints are intentionally **not**
modelled inside it and are checked after a solution exists. All of these are
**advisory** (the plan is still produced) and are printed *after* the merge list,
so the plan reads first and the caveats follow — as emerge does. Some live in the
solver crate (they read its `VersionData`), some in the cli (they need only a
package's own facts):

| Check | Where | Notes |
|---|---|---|
| USE-dep constraints (`[flag]`, `[flag?]`, `[flag=]`) | crate `validate.rs` | `check_use_deps` |
| Blockers (`!foo` / `!!foo`) | crate `validate.rs` | `check_blockers`; evaluates the blocker's own USE condition to avoid false positives |
| `::repo` constraints | crate `validate.rs` | `check_repo_constraints` |
| Reverse-dependency conflicts | cli `conflicts.rs` | complete-graph check (every installed pkg's deps vs the plan) that a default `emerge -p` skips; advisory, reported as "Dependency constraint conflict" |
| `REQUIRED_USE` | cli `required_use.rs` | **Level A** — see below |

### REQUIRED_USE: Level A vs Level C

`REQUIRED_USE` (`^^`/`??`/`||`/`flag? ( … )`) is handled at two possible levels:

- **Level A — validate & report (current).** `RequiredUseExpr::is_satisfied` /
  `unsatisfied` (in `portage-metadata`) evaluate each planned package's
  constraint against its effective USE; violations are reported as an advisory
  warning, matching Portage's default "fix your USE flags" behaviour. This is a
  purely local, post-solve check, so it lives in the cli (`required_use.rs`)
  beside `conflicts.rs` — it needs no solver state, and therefore the solver
  crate does **not** depend on `portage-metadata`.
- **Level C — solver auto-satisfaction (`--autosolve-use`, opt-in).** With the
  flag, `REQUIRED_USE` is encoded as relations between `UseDecision` packages so
  the solver *picks* satisfying flags (biased toward the configured value); the
  choices fold back into the displayed USE via synthetic `package.use`, and any
  flips are reported in a dedicated per-package report that cites the driving
  `REQUIRED_USE` clause (`output::report_autosolved_use`). Nested groups under a
  ceded guard (`a? ( ^^ ( b c ) )`) are encoded by gating, and choice branches are
  ordered toward the configured value so already-valid packages are left
  untouched. The cli cedes a package's flags **only when its `REQUIRED_USE` is
  actually violated**, and never cedes a flag pinned by `package.use` or by any
  force/mask (`ForceMask::pins`: `use.force`/`use.mask`, `package.use.force`/`mask`,
  and the `*.stable.*` variants) — so settled USE_EXPAND flags are not re-decided
  and profile-forced flags are never flipped. Intra-package only so far. It is
  **off by default**
  so default `em -p` keeps matching `emerge -p` (which does not auto-satisfy
  `REQUIRED_USE`). Concern split, the PubGrub encoding, and remaining phases are
  in [required-use-level-c.md](../portage-atom-pubgrub/docs/required-use-level-c.md).

Keeping Level A in the cli is deliberate: the `portage-metadata → portage-atom-pubgrub`
dependency is a Level-C cost, not a Level-A one.

## Solvers are interchangeable

Both solver bridges expose a `PackageRepository` trait and a provider over the
same facts; `em` defaults to PubGrub. This lets a plan be cross-checked between
two independent algorithms. The boundary rule for both: **facts in (deps, slots,
versions, IUSE names) and resolved policy in (desired USE via `desired_use`);
the solver computes the *needed* set and never resolves policy.**

## Known divergences from emerge

The plan (package set + versions) matches `emerge -p` on the test basket. The
useful way to read the remaining gaps is by **handling tier** — the guarantee a
constraint gets — not by feature, since almost everything is "handled outside the
PubGrub core" in *some* way:

- **Tier 1 — solved (enforced).** The solution provably satisfies it: version
  ranges, slots/subslots, `||`/`^^`/`??` groups, USE-*conditional* deps
  (`flag? ( dep )`), and Level-C `REQUIRED_USE` (opt-in, `--autosolve-use`).
- **Tier 2 — advisory.** Checked post-solve; the plan is still emitted even when
  violated, and the caveat is printed after it (as emerge does):
  - blockers (`!foo`/`!!foo`) — reported, not used to exclude/replace;
  - `::repo` constraints;
  - `REQUIRED_USE` Level-A (the default);
  - reverse-dependency conflicts — an *enrichment* a default targeted `emerge -p`
    hides (every installed package's constraints checked against the plan);
  - cross-package `[flag]` USE-deps — surfaced as autounmask `package.use`
    suggestions (C7 will promote these toward Tier 1 under `--autosolve-use`).
- **Tier 3 — invisible.** Not detected; the plan can silently differ from emerge
  with no warning:
  - slot-operator `:=` subslot-change rebuilds of installed dependents;
  - old-slot wrapper/shim packages (`autoconf-wrapper`, `gcc-config`).

Plus two **intentional** cosmetic divergences: install-*order* positions (valid
topological order, different scheduler — emerge: target-driven DFS; here: SCC
condensation + lexicographic Kahn) and the `:slot` suffix on autounmask
`package.use` atoms. Severity tracks the tier: Tier 3 (silent) is the priority to
fix, Tier 2 is a deliberate "report don't block" stance (some intentional like
reverse-deps, some pending promotion like blockers and cross-package `[flag]`).
The running per-item list lives in the
[`portage-atom-pubgrub` README](../portage-atom-pubgrub/README.md) "Known
limitations" section and `docs/required-use-level-c.md` (§6, C7).
