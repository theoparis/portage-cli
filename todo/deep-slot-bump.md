# `--deep` / emptytree: bump `:*` any-slot deps to the newest slot

## emerge behaviour (sandbox aarch64, clean stage3 + rust-bin-1.93.1 installed, 2026-06-18)

`www-client/firefox`, observing `dev-lang/rust-bin` (slotted by version;
consumers use the rust-eclass `|| ( >=dev-lang/rust-bin-MIN:* >=dev-lang/rust-MIN:* )`,
max MIN in the closure = `>=1.88.0` from cargo-c, satisfied by installed 1.93.1):

| emerge invocation        | total ebuilds | rust-bin pulled            |
|--------------------------|---------------|----------------------------|
| `-p`   (plain)           | 125           | none (1.93.1 satisfies)    |
| `-up`  (update, shallow) | 125           | **none**                   |
| `-uDp` (update + deep)   | 131           | **`1.94.1` NS** [1.93.1]   |
| `-pe`  (emptytree)       | 383           | `1.93.1` R + `1.94.1` NS   |

**Takeaway:** the newest-slot bump for a `:*` any-slot dep is driven by
**`--deep`** (and by `--emptytree`, which implies deep) — **not** by `--update`
alone. `-u`/`-up` leaves the satisfied installed slot in place. `--deep`
re-examines transitive `:*` deps and pulls the newest slot even when an older
installed slot already satisfies the `>=MIN`. emptytree additionally reinstalls
the installed slot (R) alongside the new one (NS).

Direct-atom sanity (no `||` wrapper): `emerge -p ">=dev-lang/rust-bin-1.74.1:*"`
already picks newest `1.94.1` (no `--deep` needed) — the OR-group/`SlotChoice`
wrapper is what makes the installed-slot preference kick in for the firefox case.

## em today

- `em -pe firefox` = **382** (vs emerge 383): em keeps only `rust-bin-1.93.1`,
  does not pull the `1.94.1` NS. This is the entire remaining firefox gap.
- Cause: `choose_version` (`portage-atom-pubgrub/src/provider/solve.rs:110-205`)
  has a "prefer the already-installed branch" heuristic for OR-group /
  `SlotChoice` virtuals — it returns the installed slot instead of falling
  through to the `max()` (newest) pick at line 207. This keeps em minimal in
  general (no gratuitous new slots) but diverges from emerge under deep/empty.
- `--deep` / `--newuse` are parsed in `cli.rs` but **not consumed** by the
  resolver. `--update` only feeds `ResolveMode::PreferInstalled` for *target
  atom* disambiguation (`query/mod.rs:64`) and never reaches the solver.

## Planned wiring

Add a "prefer newest slot" signal to the provider; in `choose_version`, when it
is set, **bypass the installed-branch preference for `SlotChoice` nodes** (slot
selection of a `:*` dep) so the dep bumps to the newest slot (`max()`):

- **off** by default and under plain `-u`  → em stays minimal (match `emerge -p`/`-up`)
- **on** under `--deep`                    → match `emerge -uDp`
- **on** under native `--emptytree`        → closes the 382 → 383 firefox gap

Keep regardless of the flag:
- the `Choice`-node **USE-dep-satisfied** branch selection (lines 140-166) —
  that's correctness (e.g. python:3.13 vs 3.14), not an update preference;
- scope the bump to `SlotChoice` (slot pick), **not** all OR-groups, so the flag
  means "prefer newest *slot*", not "re-pick providers".

Delicate code (prior `prefer_or_branch` change regressed the firefox count), so
re-run the full sandbox matrix after: `em -p`/`-up` minimal, `em -uD` → 131,
`em -pe` → 383; then benchmark + diff vs emerge.
