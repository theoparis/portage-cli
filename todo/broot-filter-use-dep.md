# broot-filter dropped a BDEPEND whose atom USE-dep forced a rebuild

STATUS: FIXED 2026-06-18. `broot_filter` is now atom-USE-dep-aware: a
`BDEPEND`/`IDEPEND` edge is host-satisfied only when the host instance's
**current** USE meets every `[flag]`/`[flag(+)]` on that edge; otherwise the
edge is kept (rebuild). `em -p app-office/libreoffice` went from **3 under-pulls
→ 0 diffs** vs emerge; the full parity basket is `RESULT: parity OK`.

## Bug

`em -p app-office/libreoffice` under-pulled 3 packages vs emerge:
`dev-libs/boehm-gc`, `www-client/w3m`, `virtual/w3m`.

## Mechanism

- `x11-misc/xdg-utils` BDEPEND `>=app-text/xmlto-0.0.28-r3[text(+)]`
  (`sys-apps/dbus` BDEPENDs `app-text/xmlto` plain — both are BDEPEND).
- xmlto is host-installed (BROOT) with `text` **off**.
- xmlto RDEPEND/DEPEND: `text? ( || ( virtual/w3m … ) )`.
- `broot_filtered` dropped the xmlto BDEPEND edge purely on a **version** match,
  so xmlto was never a solved node; em detected the `[text(+)]` violation (showed
  `text*`, would write `package.use`) but never re-expanded xmlto's `text?`
  conditional → w3m/boehm-gc silently dropped. `--autosolve-use` didn't help:
  co-solve forces the USE change but the broot-filtered edge is never a node to
  re-expand.

## PMS basis

- `[text(+)]` is a 4-style atom USE-dependency (PMS §8.3, EAPI 4 `(+)` default):
  the matched package **must** have the flag on. Installed xmlto lacks it → the
  dependency is unsatisfied → portage **re-merges xmlto with `text` on** (the
  `R` + `text*`).
- `text? ( || ( … ) )` is a USE-conditional dependency (PMS §8.2.2), evaluated
  against effective USE; with `text` now on it fires → `virtual/w3m` →
  `www-client/w3m` → `dev-libs/boehm-gc`. A rebuilt package pulls its
  re-evaluated conditional closure.

## Fix

- `host_installed` carries the host instance's active USE + IUSE (new `HostEntry`;
  read from the VDB `USE`/`IUSE` in `load_host_installed`).
- `host_satisfied_on_broot` now also checks the edge's atom USE-deps against the
  host's **current** USE (host is not rebuilt, so no rebuild-desired credit),
  reusing `eval_violated_use_dep` for parity with post-solve validation. The
  parent's desired USE (`VersionData::desired`) supplies the parent-flag state
  for `[flag?]`/`[flag=]` kinds. IUSE is required to honour `(+)`/`(-)` defaults
  for flags absent from the host package.
- Applies uniformly to native `broot_filtered` and the cross paths
  (`cross_target_runtime_deps`, `host_native_deps`) via the shared
  `append_unsatisfied_broot`.

## Relation to other work

Refines the planned broot redesign (`em-emptytree.md:56`, "normal native `/`
install → prune"): that cell is now "prune only if the host version's USE
satisfies the edge's atom USE-deps; otherwise rebuild". Architecturally adjacent
to `autounmask-convergence.md`, but the root here was broot-filtering, not the
co-solve fixpoint.

## Regression tests

`host_installed_bdepend_with_unmet_use_dep_is_rebuilt` and
`host_installed_bdepend_with_met_use_dep_is_pruned` in
`portage-atom-pubgrub/src/provider/mod.rs`.
