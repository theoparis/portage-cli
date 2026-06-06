# Design: Autounmask-driven graph completeness

Status: **Draft / proposed**
Author: depgraph work, 2026-06
Scope: `portage-atom-pubgrub` resolver + `portage-cli` depgraph driver

## 1. Problem

`em -p www-client/firefox` produces a **26-package** graph; `emerge -p
www-client/firefox` produces a **78-package** graph. We are missing ~2/3 of the
tree, and consequently the install order does not match either.

The missing packages are an entire X11 / multimedia RDEPEND closure:

```
media-libs/{freetype,harfbuzz,libvpx,libwebp,libglvnd,libaom,libass,libepoxy}
media-video/ffmpeg
x11-libs/{cairo,libX11,libxcb,libXext,libXrender,gdk-pixbuf,...}
x11-themes/{adwaita-icon-theme-legacy,hicolor-icon-theme}
dev-python/{pygments,mako,markupsafe,...}
...
```

These are not exotic packages: every one of them carries a **stable `arm64`**
keyword, an accepted license (`ACCEPT_LICENSE="* -@EULA"`), and none are masked.
`emerge` installs them happily. Our resolver drops them.

### 1.1 The misleading symptom

In verbose mode we currently print, for each dropped dep:

```
note: dropped x11-libs/cairo (no arm64 keywords)
```

This reason string is a **guess** computed in `output::report_dropped_deps`: if
the package exists anywhere in the raw tree it prints "no `<arch>` keywords",
otherwise "not in tree". It does **not** reflect the actual filter that removed
the package. cairo has a stable `arm64` keyword — the message is simply wrong,
and it has been hiding the real cause.

## 2. How resolution works today

The pipeline (in `portage-cli/src/query/depgraph/`):

1. **Load** (`repo::load_repo`) — parse the md5-cache into `RepoData`, the
   *unfiltered* set of every `(Cpv, CacheEntry)`. This is what the display layer
   reads.

2. **Filter** (`repo::Adapter::versions_for`) — for each CPN, drop versions that
   fail the keyword, `package.mask`, or license checks. The provider is built on
   top of this filtered view. **Filtering happens before the solver ever runs.**

3. **Build provider** (`PortageDependencyProvider::new`,
   `provider.rs:190`) — register the surviving versions, convert their deps, then:
   - `known` = the set of `PortagePackage` keys that have at least one
     registered version.
   - Any dependency whose target is **not in `known`** is removed from the
     constraint set and recorded in `dropped_deps` (`provider.rs:356-378`).
   So a package with zero surviving versions becomes invisible, and every edge
   pointing at it is silently severed.

4. **Solve** (`resolve_targets`) — PubGrub resolves over the pruned graph.

5. **Post-solve USE validation** (`provider.rs:635+`, surfaced as
   `use_flag_requirements`) — for packages **already in the solution**, detect
   USE-dep violations (e.g. parent needs `cairo[X]` but cairo's `X` is off) and
   record the flags that must change. This is what feeds the
   `package.use` autounmask output.

## 3. Root cause

Two independent mechanisms drop a needed package, and **neither one can ever
add it back**:

### 3.1 Filter-then-drop is terminal

Keyword/mask/license filtering in step 2 is final. If every version of a package
is filtered, the package is not in `known`, the dep is moved to `dropped_deps`,
and resolution proceeds without it. There is no feedback path that says "this
drop made the graph incomplete; relax the filter for this package and retry."
`emerge`, by contrast, treats such a situation as an **autounmask candidate**:
it assumes the keyword/mask change will be made and keeps the package in the
graph.

We already *detect* these candidates (`repo::find_autounmask_candidates`) and
print them — but purely as a **post-hoc display**. The candidate is never fed
back into a re-resolution, so the package and its entire subtree stay missing.

### 3.2 USE-dep autounmask only works for packages already in the graph

The post-solve validation in step 5 can say "cairo needs `X` flipped on" — but
**only if cairo is already in the solution**. For firefox, the autounmask USE
changes `emerge` reports are exactly:

```
>=media-libs/libvpx-1.16.0  postproc
>=x11-libs/cairo-1.18.4-r1  X
>=media-libs/libglvnd-1.7.0 X
>=media-libs/freetype-2.14.3 harfbuzz
```

These four are precisely the **roots of the missing subtree**. Because they are
dropped before the solve, they never enter the solution, the post-solve pass
never sees them, no USE requirement is generated, and their transitive closures
(harfbuzz, libwebp, ffmpeg, the libX11/libxcb chain, …) vanish with them.

> **Implementation step 0 — confirm the exact drop path.** cairo passes our
> manual keyword/license/mask checks yet appears in `dropped_deps` (which is only
> populated by the `known`-partition). That means either (a) the Adapter filter
> is removing cairo for a reason not yet identified, or (b) the dep reference
> generated for `cairo[X]` (slot operator `:=`) keys to a `PortagePackage`/slot
> that is not the one registered, so `known` misses it. Add temporary
> instrumentation to `provider.rs:356-378` printing the dropped `PortagePackage`
> (full slot key) and the `known` keys for that CPN before designing the patch.
> The solution below is robust to either answer, but the patch surface differs.

### 3.3 Order divergence is mostly a symptom

With 52 packages missing the topological order cannot match. There are two
secondary ordering issues worth fixing independently (tracked separately, not in
scope here): `install_order` only follows `DEPEND|BDEPEND` edges (ignores
RDEPEND), and `mod.rs` appends `reinstall_deps` *after* the target. Neither
matters until the graph is complete.

## 4. Goals / non-goals

**Goals**

- `em -p <target>` resolves the same package *set* as `emerge -p <target>` for
  the common case where completeness requires autounmask (keyword, mask,
  license, and USE changes).
- The autounmask changes we already print remain correct and now correspond to a
  graph that actually includes the affected packages and their subtrees.
- Deterministic termination.

**Non-goals (this document)**

- Exact install-*order* parity (separate work; depends on RDEPEND ordering).
- Download-size reporting.
- Full parity with `--autounmask-backtrack=y` exhaustive search. We target the
  default `emerge` behaviour: assume the changes, resolve once more, stop.
- Blocker/slot-conflict resolution changes.

## 5. Design: iterative autounmask re-resolution

Mirror `emerge`'s loop: resolve, collect the changes needed to make the dropped
edges satisfiable, apply them as a **relaxation set**, and re-resolve. Repeat
until no new relaxations are discovered (fixpoint).

### 5.1 The relaxation set

A value threaded into both the `Adapter` (filtering) and the
`PortageDependencyProvider` (USE evaluation / dep expansion):

```rust
#[derive(Default, Clone)]
pub struct Relaxations {
    /// CPVs to admit despite keyword failure (→ package.accept_keywords).
    pub accept_keywords: HashSet<Cpv>,
    /// CPVs to admit despite package.mask (→ package.unmask).
    pub unmask: HashSet<Cpv>,
    /// CPVs to admit despite license rejection (→ package.license).
    pub accept_license: HashSet<Cpv>,
    /// Per-(CPN, slot) USE flags to force on (→ package.use). The forced flags
    /// must be visible during dep conversion so conditional deps expand.
    pub forced_use: HashMap<(Cpn, Option<Slot>), UseForce>,
}
```

`accept_keywords`/`unmask`/`accept_license` keep a *filtered* version visible to
the solver. `forced_use` keeps a package's USE-conditional deps **and** prevents
the package from being pruned when a parent's `[flag]` dep would otherwise be
unsatisfiable.

### 5.2 Adapter changes

`Adapter::versions_for` gains access to `&Relaxations`. A version that fails the
keyword/mask/license check is **still emitted** if its CPV is in the
corresponding relaxation set. This makes the package re-enter `known`, so the
edge to it is no longer dropped.

### 5.3 Provider changes

Two changes:

1. **Forced USE at conversion time.** `provider.rs:250` computes
   `apply_package_use` per CPV. Extend the applied config with
   `relaxations.forced_use` for that `(cpn, slot)` so that:
   - the package's own USE-conditional deps under `flag?` expand (pulling the
     subtree the flag enables), and
   - the package is treated as satisfying parents' `[flag]` deps, so it is not
     dropped.

2. **Promote USE-dep violations on *droppable* targets to relaxations.** Today
   the post-solve pass only flags violations for in-solution packages. We need
   the violation for a *dep target that is about to be dropped* (cairo[X]) to
   become a `forced_use` entry for the next iteration. See 5.4 step 3b.

### 5.4 The fixpoint loop (lives in the cli driver, `depgraph()`)

Keeping the loop in the driver keeps the provider a pure function of
(repo-view, relaxations) and makes each iteration independently testable.

```
relaxations = Relaxations::default()
loop (max N iterations, e.g. 10):
    view     = Adapter { data, arch, accept_keywords, package_mask,
                         accept_license, relaxations }
    provider = PortageDependencyProvider::new(view, use_config, package_use,
                                              relaxations.forced_use)
    add installed packages
    solution = provider.resolve_targets(root_deps)?       // see 5.5 on failure

    new = Relaxations::default()

    // (3a) keyword/mask/license drops → relax the filter
    for cand in find_autounmask_candidates(data, provider.dropped_deps(), ...):
        // only for deps referenced by packages that ARE/Will be in the graph
        push cand.cpv into new.{accept_keywords|unmask|accept_license} by reason

    // (3b) USE-dep violations on dropped-or-present targets → force USE
    for req in provider.use_flag_requirements()
             + use_dep_violations_against_dropped(provider):
        new.forced_use[(req.cpn, req.slot)] |= req.required_enabled/disabled

    if new ⊆ relaxations:            // fixpoint: nothing new
        break
    relaxations |= new

emit solution + accumulated relaxations as the autounmask report/write
```

Each iteration can surface deeper requirements (forcing `cairo[X]` pulls libX11,
which may itself need a keyword relaxation), which is why we loop to a fixpoint
rather than doing a single second pass.

### 5.5 Resolution failure during iteration

A relaxed re-resolve can still fail (genuine conflict, or a relaxation made
things worse). Strategy:

- Keep the **last successful** solution + its relaxations.
- If iteration *k+1* fails, stop and emit iteration *k*'s result plus a note
  ("further autounmask changes may be required", matching emerge's
  `--autounmask-backtrack` hint).
- Hard-cap iterations to guarantee termination.

### 5.6 Strategy seam: two discoverable approaches to benchmark

The `Relaxations` abstraction makes `resolve` a **pure function of `(repo-view,
relaxations)`**. Everything downstream of relaxation discovery — the relaxed
`Adapter`, the forced-USE provider, the final solve, the report/write — is
shared. The *only* thing that varies is **how the relaxation set is produced**.
That is the seam:

```rust
/// Produces the relaxation set to feed the final resolve.
trait RelaxationStrategy {
    fn discover(
        &self,
        ctx: &ResolveCtx,           // data, arch, accept_*, package_mask, use_config, ...
        roots: &[(PortagePackage, PortageVersionSet)],
    ) -> Relaxations;
}
```

Two implementations, benchmarkable head-to-head because they share all other
machinery and emit the same `Relaxations` shape:

**A. Iterative relaxation (fixpoint).** §5.4. Resolve → collect the deps/USE
violations that became droppable *this* round → add to the set → resolve again →
stop at fixpoint. Discovers relaxations breadth-first as the graph grows.
- Cost: `K` resolves, where `K` = depth of the relaxation cascade (for firefox,
  expected ~2–4).
- Precision: relaxes **exactly** the minimal reachable set — never suggests a
  change for a package that isn't in the final graph.

**B. Single-shot hypothesis (two-pass).** Pass 1 walks the target's full
dependency closure over the *unfiltered* `RepoData` (which `load_repo` already
holds), and for every reachable package decides up front whether it would need a
keyword/mask/license/USE relaxation — building the entire `Relaxations` set in
one analysis pass. Pass 2 does a single relaxed resolve.
- Cost: **2 resolves**, independent of cascade depth.
- Risk: **over-relaxation.** The pass-1 walk must evaluate USE conditionals to
  avoid descending into branches that won't be active; a naïve closure walk will
  pull in packages (and emit autounmask changes) that the real solve would never
  select. Pruning the walk correctly is effectively re-implementing part of the
  solver's USE evaluation, and any residual over-relaxation must be reconciled
  against the final solution before emitting changes (drop relaxations for CPNs
  absent from the solve).

**What to measure.**
- Wall-clock and resolve-count on a basket (firefox, a `kde-meta`/`gnome`-class
  target, a plain leaf, a deep-cascade target).
- **Correctness**: does the emitted relaxation/autounmask set exactly match
  `emerge`'s? Approach A is the reference for minimality; B is checked for
  over-relaxation (extra suggestions) and under-relaxation (missed, if the
  pruning is too aggressive).
- Stability: same output across runs (both must sort deterministically).

**Hypothesis.** A is correct-by-construction and likely fast enough (`K` small,
each resolve already sub-3s). B trades a bounded 2-pass cost for the hard problem
of an accurate hypothesis walk; it only wins if `K` turns out large or per-resolve
cost dominates. Build the seam, implement A first (it doubles as B's correctness
oracle), then implement B and benchmark. Wire the chosen strategy behind a flag
(e.g. `PORTAGE_CLI_RELAX_STRATEGY=iterative|single`) so the benchmark harness can
select it without recompiling.

## 6. Termination & determinism

- The relaxation set is **monotonically growing** (we only ever add). Each
  iteration either adds ≥1 relaxation or hits the fixpoint and breaks. The
  universe of possible relaxations is finite (CPVs × reasons, plus a bounded set
  of `(cpn, slot)` USE forces), so the loop terminates; the iteration cap is a
  safety net, not the primary bound.
- All sets are keyed by `Cpv`/`Cpn`+slot and emitted sorted, so output is
  deterministic across runs.

## 7. Edge cases

- **`||` groups.** A dropped branch with surviving alternatives must *not*
  trigger a relaxation — the solver already picked another branch.
  `find_autounmask_candidates` already skips `!alternatives.is_empty()`; preserve
  that, and additionally skip forcing USE on a branch when a sibling branch is
  satisfiable.
- **Already-installed deps.** A dropped dep coming from an *installed* package's
  RDEPEND that is already satisfied on disk must not force a relaxation (this is
  the existing `new_needed_cpns` filter in `mod.rs`). Keep gating relaxations to
  CPNs referenced by newly-installed packages.
- **Conflicting forces.** If one parent needs `foo[bar]` and another needs
  `foo[-bar]`, record both and let the existing conflict reporting surface it
  rather than looping forever. Detect "same `(cpn,slot,flag)` forced both ways"
  and stop with a diagnostic.
- **Cycles.** Unchanged; `install_order` already appends cycle remnants
  deterministically.
- **`--autounmask` off.** When neither `--autounmask` nor `--autounmask-write`
  is set, `emerge` still *backtracks* to find a complete graph but then refuses
  with the masked-package error. Decide: either always run the loop (so the graph
  is complete and we print the same "necessary changes" error), or only run it
  when autounmask is requested. Recommended: **always run the loop** for graph
  completeness; gate only the *write* on `--autounmask-write` and the
  *suggestion print* on `--autounmask` (current behaviour), and on a successful
  relaxed solve without the flag, print emerge's "the following changes are
  required" block. This matches `emerge` semantics.

## 8. Phasing

1. **Phase 0 — confirm drop path** (§3.2 step 0). Instrument, capture the real
   reason cairo et al. leave `known`. ~half a day. *Gates the rest.*
2. **Phase 1 — relaxation seam + keyword/mask/license relaxation.** Define
   `Relaxations` and the `RelaxationStrategy` seam (§5.6); make `resolve` a pure
   function of `(view, relaxations)`. Thread the three CPV sets through `Adapter`
   and implement **strategy A (iterative)** with no USE forcing yet. Validates
   the loop machinery on the simpler filter-drop case; pick a target whose
   completeness needs only a keyword change to test.
3. **Phase 2 — USE forcing.** Add `forced_use` to the provider (conversion-time
   application + don't-drop-on-`[flag]`), and the 3b promotion of USE-dep
   violations against droppable targets. This is what unblocks firefox.
4. **Phase 3 — strategy B + benchmark.** Implement **strategy B (single-shot
   hypothesis)** behind the same seam, with A as the correctness oracle. Build
   the benchmark harness (§5.6 "What to measure"), select via
   `PORTAGE_CLI_RELAX_STRATEGY`. Decide the default from the numbers.
5. **Phase 4 — failure handling & caps** (§5.5), conflict detection (§7), and
   the `--autounmask`-off semantics (§7 last bullet).
6. **Phase 5 — validate against `emerge -p`** for a basket of targets; diff
   package sets to zero.

## 9. Testing

- **Unit (provider):** in-memory repo where package B is keyworded-out and A
  deps on B; assert one resolve drops B, and a resolve with `B@ver` in
  `accept_keywords` includes B and its deps.
- **Unit (USE forcing):** A deps `B[x]`, B's `x?` pulls C; assert forcing `B[x]`
  pulls both B and C; assert the emitted `forced_use` equals `{(B,_): +x}`.
- **Unit (fixpoint):** chain where forcing `B[x]` exposes C which is
  keyworded-out; assert the loop runs 2 iterations and ends with both
  relaxations.
- **Unit (termination):** conflicting `foo[bar]`/`foo[-bar]`; assert it stops
  with a diagnostic, no infinite loop.
- **Integration:** `em -p www-client/firefox` package set == `emerge -p`
  package set (compare sorted CPN lists; allow a documented allowlist for
  genuinely environment-specific diffs).

## 10. Output / UX impact

- The autounmask report becomes *consistent* with the graph: every suggested
  change corresponds to a package now present in the merge list.
- `Total:` counts grow to match emerge.
- The misleading "no `<arch>` keywords" note (§1.1) should be replaced with the
  *actual* relaxation reason now that we track it per CPV; or dropped entirely in
  favour of the structured autounmask block.

## 11. Open questions

- Exact `known`-exclusion path for cairo (Phase 0).
- Should `forced_use` be keyed by `(cpn, slot)` or by `Cpv`? Slot is enough to
  match `package.use` atoms but per-version forcing may be needed if different
  versions have different IUSE. Lean `(cpn, slot)` + version range on emit.
- Do we need to relax in dependency order, or is the unordered fixpoint
  sufficient? Conjecture: unordered is sufficient because each iteration
  re-resolves the whole graph; confirm in Phase 2.
- Interaction with the pre-existing single-installed-package `NoSolution` bug
  (jq/zstd/coreutils) — likely orthogonal, but re-test after Phase 1.
```
