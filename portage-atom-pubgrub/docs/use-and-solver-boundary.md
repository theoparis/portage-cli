# Design: USE concerns and the solver boundary

Status: **Implemented** (2026-06-07)
Scope: `portage-atom-pubgrub` (+ the `portage-cli` Adapter that feeds it)

## Implementation status

- ✅ **Step 1** — `PackageData` → `BTreeMap<Version, VersionData>` (map-of-structs).
- ✅ **Step 2** — characterization tests for the autounmask "needed" semantics.
- ✅ **Step 3a** — single per-version `desired` set (`VersionData::desired`);
  `effective_flag_new` reads it; provider's stored `package_use` removed.
- ✅ **Step 3b** — `desired` computation moved to the caller via
  `PackageRepository::desired_use`. `new(repo)` drops its USE params; the
  provider's global `use_config` field is gone (`use_dep_branch_satisfied` reads
  per-version `desired`); `apply_package_use` resolution now lives in the cli
  Adapter / `InMemoryRepository`. No profile/make.conf/package.use/ACCEPT_*
  resolution remains in the crate (verified: only explanatory comments mention them).
- ✅ **Step 4** — blockers + repo-constraints wired into `em -p`. `check_blockers`
  now evaluates the blocker's USE condition against the matched package's
  `desired` (no more false positives like `!glibc[crypt(-)]`) and dedups.
- ✅ **Step 5** — `SolverDecided` documented as experimental/dormant in `lib.rs`.
- ✅ **Bonus** — fixed pre-existing post-solve ordering nondeterminism
  (`use_flag_requirements` and `package.use` entries were HashMap-ordered).

## Self-review notes

- ✅ `apply_package_use` moved from `provider/mod.rs` to `use_config.rs` (its
  natural home); re-export path unchanged.
- ✅ `convert_deps`'s redundant `iuse_defaults` argument dropped — `desired`
  arrives with defaults folded (`UseConfig::fold_iuse_defaults`), so convert
  reads `use_config.get(flag)` directly.
- (kept) The two post-solve loops in `compute_use_flag_requirements` were **not**
  collapsed: they are not pure duplicates (the second is the
  upgrade-instead-of-rebuild cascade over installed packages). The spec
  overstated this; both now read the single `desired` source, which was the goal.
- (kept, minor) `desired_use`'s IUSE-default fold is shared via
  `UseConfig::fold_iuse_defaults` for `InMemoryRepository`, but the cli/benchmark
  Adapters inline the loop (they read `portage_metadata::Iuse`, a different shape).

This is the spec for consolidating USE handling and cleaning the crate's
responsibilities. It supersedes the "canonical effective-USE resolver" idea
floated during review, which was wrong: it would have pulled policy resolution
into the solver.

## 1. The boundary principle

`portage-atom-pubgrub` is a **solver over facts**. Exactly two kinds of input
cross its boundary, and it computes neither of them:

- **Intrinsic ebuild facts** (in scope): deps, slots, versions, IUSE *names*.
- **Resolved policy** (caller's scope, handed in): anything that required a
  profile, `make.conf`, `package.use`, `ACCEPT_KEYWORDS`, IUSE defaults, or the
  VDB to compute.

"Effective USE" — `profile ∘ make.conf ∘ package.use ∘ IUSE-defaults ∘
use.force/mask` — is **policy resolution** and belongs entirely to the caller
(`build_use_env` + the `PackageRepository` Adapter). The solver must never
recompute it.

> The over-eagerness bug fixed on 2026-06-06 (`effective_flag_new`, stored
> `package_use`, `apply_package_use` in the provider) was a symptom of policy
> leaking across this boundary. The proper fix is to move it back out, not to
> make the in-crate copy smarter.

## 2. The three USE sets

All per-package; membership means "this flag is ON".

| set | meaning | produced by | enters the crate as |
|---|---|---|---|
| **current** | what is installed (as-built) | VDB | `InstalledPackage.active_use` (already an input) |
| **desired** | what our USE var wants | caller, fully resolved (incl. `package.use` + IUSE defaults) | **new** `PackageRepository::desired_use(&Cpv)` |
| **needed** | what the chosen solution requires | the solver / post-solve pass | crate-computed (`UseFlagRequirement`) |

The crate's only USE responsibility is to **compute `needed`** and express its
results as **set arithmetic** over the three. It never recomputes `desired`.

`desired` enters via a **separate trait method**, not folded into
`PackageVersions`, to keep facts and resolved-policy visibly distinct at the
boundary:

```rust
trait PackageRepository {
    fn all_packages(&self) -> Vec<Cpn>;
    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)>; // facts
    fn desired_use(&self, cpv: &Cpv) -> UseStates;                    // resolved policy
}
```

`UseStates` is `flag -> UseFlagState` (`Enabled | Disabled | SolverDecided`).
If the second lookup per version proves to be a measurable cost, fold it into
`PackageVersions` later — the trait shape is the only thing that changes.

## 3. Everything post-solve is set arithmetic

With `desired` authoritative, the post-solve outputs stop re-deriving anything:

- **autounmask `package.use`** = `needed \ desired`.
  `package.use` and IUSE defaults are already inside `desired`, so this is a
  plain difference. (Structurally removes the python_targets / `glib` / `png`
  over-reports — no `effective_flag_new` special-casing.)
- **rebuild** (`[ebuild R]` + `*` USE markers) = installed ∧
  `current ≠ (desired ∪ needed)`.
- **install (`N`)** = not in `current`; flags shown are `desired ∪ needed`.

### Installed-ness only chooses the baseline

`needed` is computed **identically** for installed and new packages — it is just
"the USE-dep requirements the solution implies". Whether a package is installed
only selects *which set we diff `needed`/`desired` against*:

- installed → diff against **current** to detect rebuilds;
- any → diff `needed` against **desired** to detect autounmask.

This collapses the two near-duplicate loops in
`compute_use_flag_requirements` into one.

## 4. `SolverDecided` is not a special case

A flag the caller cedes to the solver is simply a flag whose `desired` value is
`SolverDecided` instead of `Enabled`/`Disabled`. The crate already encodes such
flags as `UseDecision` virtual packages (v0=off / v1=on); PubGrub's
one-version-per-package rule gives mutual exclusion for free.

Today the cli never emits `SolverDecided` (it hands the solver a fully-fixed
USE var), so the `UseDecision` path is **dormant but preserved**. It is the
crate's strategic lever to eventually beat portage on:

- **REQUIRED_USE satisfaction** — `^^ () / ?? () / a? ( b )` encoded as
  relations between `UseDecision` packages and solved directly rather than
  erroring. *(Not built: REQUIRED_USE is not parsed in `portage-atom` yet.)*
- **minimal-USE-change conflict resolution** — co-solving flags + versions +
  slots instead of one-shot autounmask backtracking.

Both require, before they are useful: (a) the fixed-USE mode matching portage
well (the baseline / oracle), and (b) a preference model so solver-chosen USE
stays minimal and predictable (bias `choose_version`/`prioritize` and the v0/v1
ordering toward the configured value). Until then, keep the path intact and
documented as experimental; do not activate it.

## 5. What moves, what stays

**Moves out of the crate (into the cli Adapter):**
- `apply_package_use` and the provider's stored `package_use`.
- IUSE-default application (the caller folds `+flag` into `desired`).

**Stays in the crate:**
- IUSE *names* (validity, and `(-)`/`(+)` dep-default handling for flags outside
  a target's IUSE).
- The single violation primitive `eval_violated_use_dep` (the six `UseDepKind`
  cases).
- `UseFlagState` (the type) and the `UseDecision` encoding.

**Collapses to one implementation:**
- `convert` (branch walking), `validate::check_use_deps`, and the provider
  post-solve pass all read `desired` from the one trait method. The three
  divergent "is this flag on" notions become one; the divergence that caused the
  IUSE-default bug cannot recur.

## 6. `validate.rs` becomes load-bearing, not dead

`validate.rs`'s public API (`check_use_deps`, `check_repo_constraints`,
`check_blockers`, `slot_operator_bindings`) is currently reachable only from its
own tests. It is kept and made canonical:

- its USE check delegates to the shared `desired` reader + `eval_violated_use_dep`;
- **blockers are wired into the live pipeline** — `em -p` (and the future
  `em build` and build-plan comparison tooling) must honour `!foo` / `!!foo`.
  Today the solver does not model blockers and `conflicts.rs` explicitly skips
  `dep.blocker`; the cli will call `check_blockers` post-solve and surface the
  results like slot conflicts.

## 7. Migration sequence (each independently committable)

1. **Data model.** `PackageData`: eight parallel `BTreeMap<Version, _>` →
   `BTreeMap<Version, VersionData>`. Split `provider.rs` into ingestion /
   solving / post-solve. No behaviour change. (Removes the four `PackageData {}`
   literals that must currently be hand-synced.)
2. **Characterization tests.** Pin current `check_use_deps` / `check_blockers` /
   autounmask output *before* touching behaviour — the safety net for "no loose
   ends".
3. **Concern extraction.** Add `desired_use` to `PackageRepository`; move
   `apply_package_use` + IUSE-default application into the cli Adapter; delete
   the provider's `package_use`/`effective_flag_new`; rewrite post-solve as set
   arithmetic with a single `desired` reader; collapse the two loops.
4. **Blockers.** Wire `check_blockers` (+ `check_repo_constraints`) into the cli
   post-solve; stop `conflicts.rs` skipping blockers.
5. **lib.rs.** Document `SolverDecided`/`UseDecision` as experimental-preserved.

## 8. Invariants to hold (acceptance)

- `em -p` / `em -pv` output for the firefox basket is unchanged by steps 1–4
  (the characterization tests prove it).
- No `profile` / `make.conf` / `package.use` / `ACCEPT_*` string appears in
  `portage-atom-pubgrub` after step 3.
- Exactly one function computes "is flag F on for package P": the `desired`
  reader (for fixed flags) feeding `eval_violated_use_dep`.
- The `UseDecision` path still compiles and its unit tests still pass.

## 9. Downstream status (2026-06-08)

Post-implementation follow-ups in the consumer (`portage-cli`), completing the
"`validate.rs` becomes load-bearing" goal (§6) and verifying parity:

- **Blockers + reverse-dep constraints are live in `em -p`.** `check_blockers`
  and `check_repo_constraints` run post-solve; the consumer additionally runs a
  complete-graph reverse-dependency check (every installed package's constraints
  vs the plan). It is **non-fatal/advisory** — the plan is still produced.
- **Conflict report relabelled.** That reverse-dep check was previously printed
  under a "Slot conflict" header; it is not slot-specific, so it now reads
  "Dependency constraint conflict". (e.g. it surfaces upgrading `docutils` past
  an installed package's `<` bound — real breakage that a plain `emerge -p`
  leaves silent because emerge doesn't pull reverse deps into a targeted graph.)
- **Tree-view dedup fix.** `em query depgraph --format tree` now dedups children
  by package value (a package reached via two dep classes produces non-adjacent
  duplicate edges; the old positional `dedup_by_key` missed them).
- **Verified parity.** `em -p` matches `emerge -p` on the full *versioned*
  package set for the firefox / texlive-core / qtbase basket. Remaining
  divergence is install-*order* positions only. The current running gap list and
  the performance comparison live in the crate `README.md`.
