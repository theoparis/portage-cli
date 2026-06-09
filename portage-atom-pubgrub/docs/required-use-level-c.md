# Design: Level-C `REQUIRED_USE` (solver auto-satisfaction)

Status: **Proposed** (2026-06-08) — design only, not implemented.
Scope: `portage-atom-pubgrub` (+ the `portage-cli` Adapter that feeds it).
Builds on: [`use-and-solver-boundary.md`](use-and-solver-boundary.md) §4.

> **Slop warning.** This codebase is largely AI-generated. Verify a claim
> against the code before relying on it; update this file when it drifts.

## 0. Decisions taken

- **Opt-in, default off.** Default behaviour stays **Level A** (validate &
  report) so `em -p` keeps matching `emerge -p` byte-for-byte — `emerge` does
  *not* auto-satisfy `REQUIRED_USE` by default (it errors: "fix your USE
  flags"). Level C is gated behind an explicit flag (working name
  `--autosolve-use`). When `em` flips a flag, it surfaces it through the
  existing autounmask `package.use` path, not silently.
- **Intra-package only (first cut).** Encode each package's own
  `REQUIRED_USE` over *its* `UseDecision` nodes. Cross-package `[flag]`
  USE-deps stay a post-solve check, as today. Co-solving them is a later,
  larger phase (§6).

## 1. Mental model: A vs C

| | Who decides each flag's value | `REQUIRED_USE` |
|---|---|---|
| **Level A** (today) | caller, fully fixed (`desired_use`) | checked *after* the solve; violations are advisory warnings |
| **Level C** | caller fixes *most*; **cedes some to the solver** | encoded as solver *constraints*; the emitted plan is `REQUIRED_USE`-valid by construction |

A ceded flag is simply one whose `desired` state is `SolverDecided` instead of
`Enabled`/`Disabled`. The crate already turns such a flag into a `UseDecision`
virtual package with versions `0` (off) / `1` (on), and conditional deps gated
on it already fire (`convert.rs`, the `UseFlagState::SolverDecided` arm).
Three things are missing and are the whole of this work:

1. the caller never emits `SolverDecided` (it hands a fully-fixed set);
2. `REQUIRED_USE` is not encoded *between* `UseDecision` nodes;
3. `choose_version` has no preference, so every ceded flag would resolve to `1`.

## 2. Concern separation (the load-bearing rule)

The boundary is **"caller resolves policy; the solver consumes facts + resolved
policy and never resolves policy."** Level C keeps it intact by splitting along
exactly this line:

| Layer | Owns | Level-C responsibility |
|---|---|---|
| **portage-metadata** | the *fact grammar* | `RequiredUseExpr` + Level-A evaluator (`is_satisfied`/`unsatisfied`). Already done; no solver knowledge. |
| **portage-cli Adapter** | *policy* | decides **which** flags to cede (`SolverDecided`) and the **preferred** value; passes `RequiredUseExpr` through as a fact. |
| **portage-atom-pubgrub** | *fact-solving* | **encodes** `RequiredUseExpr` into constraints over `UseDecision` nodes and **chooses** values biased by the caller's preference. Never decides which flags are free, never decides the preference. |

So: *"which flags are free + what value I'd prefer"* is policy → stays in the
cli. *"what flag values are legal given `REQUIRED_USE`"* is fact-solving → stays
in the crate. This is precisely why `RequiredUseExpr` is threaded into
`VersionData` **only at Level C** — it becomes a solver input only once the
solver is choosing. At Level A it is purely the cli's post-solve oracle and the
crate stays free of it.

### Which flags does the cli cede?

Policy, in the Adapter, only under `--autosolve-use`:

- candidate = a flag **named in the package's `REQUIRED_USE`** …
- … that is **not pinned** by `use.force`/`use.mask` or an explicit
  `package.use` entry (those are hard user/profile decisions — never cede them);
- the **preference** handed alongside = the value the normal policy stack
  (`profile ∘ make.conf ∘ USE ∘ IUSE-default`) would have produced.

Everything else stays `Enabled`/`Disabled` exactly as in Level A. With the flag
off, *nothing* is ceded and behaviour is identical to today.

## 3. Encoding `REQUIRED_USE` in PubGrub

PubGrub expresses "selecting version *v* of *P* implies requirement *R*" —
Horn-ish implications, not arbitrary boolean CNF. `REQUIRED_USE` maps onto that
by reusing the two mechanisms already in the crate: **implication deps** and
**Choice virtuals**. For a package `P`, let `D_x` be its `UseDecision` node for
flag `x` (`D_x@1` = on, `D_x@0` = off):

| `REQUIRED_USE` | Encoding |
|---|---|
| `a? ( b )` | `D_a@1` requires `D_b ∈ {1}` |
| `!a? ( b )` | `D_a@0` requires `D_b ∈ {1}` |
| `a? ( !b )` | `D_a@1` requires `D_b ∈ {0}` |
| `\|\| ( a b c )` | a Choice virtual whose branches each require one of `D_a@1`/`D_b@1`/`D_c@1` (identical to OR-dep groups) |
| `?? ( a b c )` | pairwise mutual exclusion: `D_a@1` req `D_b ∈ {0}`, `D_a@1` req `D_c ∈ {0}`, `D_b@1` req `D_c ∈ {0}` |
| `^^ ( a b c )` | `\|\|` **and** `??` combined |
| `a? ( ^^ ( b c ) )` | the inner constraints made conditional on `D_a@1` (nested virtual) — the fiddly case, Phase 2 |

Key correctness detail: `UseDecision_{cpn}_{flag}` is **package-scoped** (it is
already named that way). The *same* node is shared between the conditional-dep
encoding and the `REQUIRED_USE` encoding, so if `REQUIRED_USE` forces a flag on,
the deps gated on that flag fire automatically. That sharing is what makes this
one problem rather than two disconnected ones.

A flag that appears in `REQUIRED_USE` but is **not** ceded (fixed by the caller)
needs no `D_x` node: its value is a constant, so each clause it appears in is
partially evaluated at encode time (a satisfied disjunct drops the whole `||`;
an unsatisfiable conjunct is left for the Level-A reporter — Level C does not
override a user's hard pin).

## 4. Preference / minimality

`choose_version` currently returns `candidates.max()`, so a `UseDecision` with
`{0,1}` always picks `1`. Level C needs the ceded flag to default to the
caller's preferred value and deviate only when a constraint forces it:

- bias `choose_version` (and/or `prioritize`) for `UseDecision` nodes toward the
  preferred version; PubGrub backtracks off it only under constraint pressure.
- this is a **greedy "keep configured unless forced"** model — predictable and
  cheap. It is *not* global minimal-flip optimisation (PubGrub is a solver, not
  an optimiser); that is explicitly out of scope.

The preference must reach the crate without re-introducing policy: extend the
ceded state to carry it, e.g. `SolverDecided { prefer: bool }` (or a parallel
`prefer: BTreeMap<flag,bool>` on the version), set by the Adapter.

## 5. Reporting

Solver-chosen values that differ from what the user configured are exactly
`needed \ desired`: they fold back into the displayed USE via synthetic
`package.use` entries (and a `*` rebuild marker when they change an installed
package). On top of that, `em` prints a dedicated **autosolve report**
(`report_autosolved_use`) grouped per resolved `cpv`, showing each flip with the
value the user had configured and the *specific* `REQUIRED_USE` clause that drove
it (`RequiredUseExpr::clauses` filtered by `mentions`, so a large constraint like
qtbase's shows only the relevant `?? ( journald syslog )`, not the whole tree):

```
*** --autosolve-use adjusted USE flags to satisfy REQUIRED_USE:

  dev-qt/qtbase-6.11.1
    -syslog  (configured on)
    because: ?? ( journald syslog )
```

The Level-A reporter remains for *un*satisfiable constraints (a hard pin that no
legal assignment can meet).

## 6. Cross-package `[flag]` USE-dep co-solve (C7) — design

This is the next phase. It promotes a cross-package `[flag]` USE-dep from an
**advisory** (the cli reports "add `Q a` to `package.use`") to an **applied**
flag under `--autosolve-use`, mirroring Level-C `REQUIRED_USE`.

### Handling tiers (the frame)

Every constraint sits in one of three tiers by the guarantee it gets:

- **Tier 1 — solved/enforced:** version ranges, slots/subslots, `||`/`^^`/`??`,
  USE-*conditional* deps (`a? ( dep )`), and Level-C `REQUIRED_USE` (opt-in).
- **Tier 2 — advisory:** checked post-solve, plan still emitted even if violated
  — blockers, `::repo`, `REQUIRED_USE` Level-A, reverse-dep conflicts, and
  **cross-package `[flag]`** (today: surfaced as autounmask `package.use`).
- **Tier 3 — invisible:** not detected, plan silently differs from emerge — `:=`
  subslot rebuilds, old-slot wrapper/shim packages.

C7 moves cross-package `[flag]` from Tier 2 toward Tier 1 (opt-in).

### Foundation that already exists

`PortageDependencyProvider::compute_use_flag_requirements` already computes, per
package, the `required_enabled` / `required_disabled` flags that the in-plan
`[flag]` deps demand (this is what the cli turns into autounmask suggestions via
`use_flag_requirements()`). So the *detection* is done; C7 only adds the
*application*.

### Approach: cede-the-target + re-solve (no new solver encoding)

Under `--autosolve-use`, after a solve:

1. Read `use_flag_requirements()`.
2. For each target package + required flag that is **a real IUSE flag** and **not
   pinned** (`package.use` / `ForceMask::pins`) and **not contradictory**, cede
   that flag on the target toward the required value.
3. Re-solve to a bounded fixpoint (the same pattern `resolve_targets` already uses
   for `upgrade_to`), so newly-ceded flags' conditional deps and `REQUIRED_USE`
   are folded in. Fall back to the last good plan if a re-solve fails.

This reuses the Level-C cede mechanism and the existing re-solve loop, keeps the
solver crate free of a new cross-package encoding, and stays gated so default
`em -p` keeps matching `emerge -p` (which also only *advises* USE changes).

### Corner cases (spec: `portage-cli/src/query/depgraph/c7.rs`)

Minimal package sets, written before the implementation as the executable spec /
future regression tests. They currently assert Tier-2 behaviour and document the
C7 target inline:

| Case | Scenario | C7 target |
|---|---|---|
| CC1 | `foo[bar]`, bar off | cede bar on |
| CC2 | `foo[-bar]`, bar on | cede bar off |
| CC3 | `foo[bar?]`, parent bar on | cede to match the active conditional |
| CC4 | `foo[bar=]` | cede foo's bar to equal the parent's |
| CC5 | `foo[bar]` vs `?? ( bar baz )` | C7 + Level-C must agree in one re-solve |
| CC6 | `[bar]` vs `[-bar]` (two parents) | detect both, stay advisory, never loop |
| CC7 | `[bar]`, bar ∉ foo's IUSE | never cede (cannot apply) |

**Findings from the spec:**
- **CC5** — C7 and Level-C interact: ceding `bar` on may break the target's
  `REQUIRED_USE`, so both must be resolved in the same re-solve.
- **CC6** — current contradiction reporting is **lossy**: only one side of a
  `[bar]`/`[-bar]` conflict is recorded in `use_flag_requirements`. C7's
  conflict-detection (don't cede when both directions are demanded) needs both
  sides, so this is fixed as part of C7 (or the follow-up cleanup pass).

### Later / out of scope

- **Nested ceded-guard chains** (`a? ( b? ( c ) )`, both ceded) — needs a
  2-antecedent implication PubGrub Horn clauses can't express (C6).
- **Per-slot `UseDecision` nodes** — kept as a Tier-2 advisory edge rather than a
  node rename, because per-slot naming conflicts with cross-package references
  from unslotted deps (C5).
- **Global minimal-flip optimisation** — not planned.

## 7. Phasing

- **Phase 0** ✅ *(done 2026-06-08)* — thread the `REQUIRED_USE` fact through to
  the provider as a *dormant* input. A crate-local `RequiredUse` enum (interned
  flags) carries it; the cli Adapter translates `portage_metadata::RequiredUseExpr`
  into it (`translate_required_use`), so the solver crate stays decoupled from the
  md5-cache parser. Field on `PackageVersions` (fact in) and `VersionData`
  (stored, unread). `required_use_is_dormant_phase0` proves inertness (an
  unsatisfiable `REQUIRED_USE` neither breaks the solve nor changes the
  solution); basket still matches `emerge -p` (0 diffs). *Not yet done:* the
  preference channel on the ceded state — deferred to Phase 1 where it is first
  needed.
- **Phase 1** ✅ *(done 2026-06-08)* — three sub-steps:
  - **1a** — preference channel: `SolverDecided { prefer }`; `choose_version`
    biases each `UseDecision` node toward the preferred value
    (`use_decision_prefer`).
  - **1b** — `convert::encode_required_use`: `a?()/!a?()/||/^^/??`/bare-flag over
    the package's `UseDecision` nodes (implications + `Choice` + pairwise
    exclusion), fixed flags partially evaluated; `register_virtual_choices`
    merges nodes so one node per `(cpn, flag)` is shared with conditional deps.
  - **1c** — cli `--autosolve-use` (off by default): the Adapter cedes each
    package's non-pinned `REQUIRED_USE` flags (preference = resolved value); the
    solver's choices are captured (`CededFlag` / `solved_use_decisions`), folded
    back into the effective USE for display + the Level-A check + autounmask via
    synthetic `=cpv flag` `package.use` entries, and flipped flags are reported.
    A failed autosolve solve falls back to a fixed-USE (Level-A) plan.

  Verified: basket parity unchanged with the flag off (0 diffs);
  `USE="journald syslog" em -p --autosolve-use dev-qt/qtbase` disables `syslog`
  to satisfy `?? ( journald syslog )` and reports the flip (no Level-A warning).

  **Cede-policy caveats (Phase 2 refinements):** flags pinned by `use.force` /
  `use.mask` are not yet distinguished from profile defaults, so they may be
  ceded (usually harmless — forced flags rarely sit in fixable `REQUIRED_USE`);
  the `UseDecision` node is per-`(cpn, flag)`, so a multi-slot package's slots
  share one decision.
- **Phase 2** ⚙️ *(in progress, 2026-06-09)*:
  - **Latent `Ord` bug fixed (load-bearing).** `PortagePackage::Ord` collapsed
    all `UseDecision` (and `Choice`/`SlotChoice`) nodes to `Ordering::Equal`,
    inconsistent with its `Eq`/`Hash`. The encoder's `touched` `BTreeSet` keyed
    on these silently kept only *one* node, so for any package with a multi-flag
    `REQUIRED_USE` the encoder dropped most of its constraints — Phase 1 was only
    ever exercised on the rare single-flag case. `Ord` now breaks ties by the
    interned name; `internal_nodes_order_consistent_with_eq` guards it.
  - **Nested groups under a *ceded* guard** (`a? ( ^^ ( b c ) )`, `a? ( || … )`,
    `a? ( ?? … )`, `a? ( b(fixed)? ( c ) )`). `convert::guarded`/`guarded_any`/
    `guarded_at_most`/`imply_choice` *gate* the body's constraints behind
    `guard@active` (a `Choice` pulled only from the guard's version bucket), so an
    inactive guard removes the constraint structurally — no escape-literal heuristics.
    A nested conditional whose *inner* guard is also ceded is deferred to Level A
    (a two-antecedent implication PubGrub Horn clauses can't model).
  - **Preference-ordered choice branches.** `Operand::Free` now carries
    `prefer_ver`; `at_least_one`/`imply_choice` order branches preference-satisfied
    first (`order_by_preference`) so the solver meets a clause without a flip when
    it already can. Fixes gratuitous flips (`||`/`^^` previously enabled the
    first-listed flag regardless of preference, e.g. swapping `PYTHON_SINGLE_TARGET`
    or adding an extra `PYTHON_TARGETS`).
  - **Cede gated on actual violation (cli).** The Adapter cedes a package's flags
    *only* when its `REQUIRED_USE` is currently unsatisfied by the resolved config
    (`RequiredUseExpr::unsatisfied`). An already-satisfied package is left fixed —
    nothing to autosolve — so the solver no longer re-decides settled USE_EXPAND
    flags (LLVM_SLOT/PYTHON_TARGETS) and drags in their conditional deps. This cut
    the `dev-qt/qtbase --autosolve-use` closure from 75 back to 42 (baseline 41),
    matching emerge's "act only on violations" behaviour.

  Verified on the live tree: no-autosolve basket package set matches `emerge -p`
  (firefox/texlive-core/qtbase); `USE="journald syslog" … --autosolve-use
  dev-qt/qtbase` still disables `syslog` and reports the one flip.

  - **`use.force`/`use.mask`-aware cede (cli).** The cli now threads the profile
    stack's `use.force`/`use.mask` (global) and matching `package.use.force`/
    `package.use.mask` into the Adapter and never cedes a flag they pin — a forced
    flag stays a fixed `Enabled`/`Disabled` operand the encoder partially
    evaluates, so the solver can't produce a plan that flips a profile-forced
    flag. Verified by `forced_flag_is_not_ceded` (+ `unforced_flags_are_ceded`,
    `satisfied_constraint_cedes_nothing`).

  - **Richer reporting (cli).** `report_autosolved_use` now groups flips per
    resolved `cpv`, prints the configured value each flag was moved away from, and
    cites only the `REQUIRED_USE` clause(s) that mention a flipped flag
    (`RequiredUseExpr::clauses`/`mentions`) rather than the whole constraint. See
    §5 for the format.

  - **`/etc/portage/profile` site layer (portage-repo).** `ProfileStack` now
    appends Portage's `/etc/portage/profile` as the top profile layer
    (`with_user_profile`), and `read_lines` reads PMS 5.2.4 directory-form profile
    files (`package.use.mask/<name>`). So site-local `use.force`/`use.mask`/
    `package.use.{force,mask}` pins flow through the existing accessors and the
    cede gate honours them — closing the gap where a site-forced flag could be
    ceded/flipped. Basket parity unchanged (the site layer here only pins
    `crossdev` packages).

  - **Per-package force/mask applied to effective USE (cli `force_mask.rs`).**
    Previously only *global* `use.force`/`use.mask` reached effective USE, and the
    per-package sets only gated ceding. Now `ForceMask` resolves the full policy
    per package — `package.use.force`/`mask` always, the `*.stable.*` variants when
    the version is merged due to a stable keyword (`is_stable`, mirroring Portage's
    `KeywordsManager.isStable`; inert on `~arch`) — and applies it (force enables,
    mask disables, mask wins) in **both** `desired_use` (the solver view) and the
    `mod.rs` display fold. This is what makes crossdev's `cross-*`
    `multilib`/`cet`/`nopie` pins take effect. The cede gate's never-cede set is
    now `ForceMask::pins`.

  *Still pending in Phase 2:* per-slot `UseDecision` nodes (a multi-slot package's
  slots share one decision); nested *ceded-guard chains* (deferred to Level A).
- **Phase 3** (maybe) — cross-package USE-dep co-solve (§6).

## 8. Invariants to hold (acceptance)

- With `--autosolve-use` **off**, `em -p`/`-pv` output is **byte-identical** to
  pre-Level-C on the firefox/texlive-core/qtbase basket (parity preserved).
- No `profile`/`make.conf`/`package.use`/`ACCEPT_*` resolution appears in the
  crate — the cede decision and the preference are *inputs*, not crate-computed.
- A package whose `REQUIRED_USE` is already satisfied by `desired` produces the
  **same** `UseDecision`-free encoding as today (Level C is inert when nothing
  needs deciding).
- The `UseDecision` node for a flag is shared between conditional-dep and
  `REQUIRED_USE` encodings (one node per `(cpn, flag)`).
- Under `--autosolve-use`, a package with an *unsatisfiable* `REQUIRED_USE`
  (forced by a hard pin) still produces a plan + the Level-A advisory, never a
  hard solver error.
