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
`needed \ desired` — the **existing autounmask `package.use` path**. No new
report surface: a Level-C flip shows up as a `package.use` suggestion (and a `*`
rebuild marker when it changes an installed package), which is the honest "I had
to change these to satisfy `REQUIRED_USE`" message. The Level-A reporter remains
for *un*satisfiable constraints (a hard pin that no legal assignment can meet).

## 6. Out of scope (later phases)

- **Cross-package USE-dep co-solve.** If `P` deps `Q[a]` and `Q.a` is ceded, the
  `[a]` should *force* `D_{Q}_a@1` during the solve. Today USE-deps are checked
  post-solve. Routing them through `UseDecision` co-solves flag choice with
  USE-dep satisfaction — strictly bigger, touches the whole post-solve model.
  Phase 1 keeps USE-deps post-solve; a ceded flag that a `[a]` later wants on
  but the solver left off is reported by the existing USE-dep validation, same
  as a fixed flag would be.
- **Nested conditional groups** (`a? ( ^^ ( … ) )`) — Phase 2.
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
- **Phase 2** — nested conditionals; richer reporting.
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
