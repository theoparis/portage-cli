# Parser audit pass

STATUS: 🔴 not started (2026-06-28).

A burst of parser-touching work landed across the metadata/profile/atom path
without a unified correctness re-check. Make a deliberate pass to confirm each
parser is PMS/`make.conf(5)`-faithful and that the layers agree, before the next
round of features piles on top.

## Scope (the parsers to review)

Recent commits (`41c35ad` `b6accf2` `1f5c6a4` `26fa1d7` `bb90bd4` `a934c89`
`2796f95` `6b2296c` `c826528` `99c9ae3` `67068eb` + the `-*` clear-all cluster)
touched:

- **Incremental `-*` clear-all** across the layers — `USE`, `ACCEPT_LICENSE`,
  `ACCEPT_KEYWORDS`, USE_EXPAND colon form (`L10N: -* en`). Confirm the
  `-*`-inside-a-group "clear then rebuild" rule and the profile→globals→conf→env
  precedence agree with `make.conf(5)` / PMS 5.2.4 everywhere, not just the depgraph
  display path.
- **`package.use` / `package.license` / `package.accept_keywords`** — the profile
  stack + `/etc/portage` readers. Are directory-form (PMS 5.2.4, files
  concatenated in filename order) and the per-package atom match identical across
  the three? `read_lines` is shared — verify it handles both file and dir.
- **`ACCEPT_LICENSE` `@GROUP` expansion** (`license_groups`) — confirm `@`-group
  resolution and the `-`-prefixed negation in license tokens parse like portage's
  `_license_map`.
- **`@set` expansion** (`@system`/`@world`/`@profile`/`@selected` + user sets) —
  the set stack and `sets.conf` reader. Verify nested `@set` refs and the
  profile `packages` accumulator.
- **USE-dep evaluation** (`UseFlagLookup` trait, interned flag keys) — the
  `[flag?]`/`[flag=]`/`[flag]` conditional eval in the atom solver bridge.
  Cross-check the `flag?` (conditional) vs `flag=` (required) semantics against
  PMS 8.2.
- **IUSE defaults** (`+flag`/`-flag`) — the `1f5c6a4` override rule and the
  `expand_use_expand_colon` group handling. Confirm the merge path and the depgraph
  path fold defaults identically (a known historical divergence risk).
- **`make.conf` / `make.globals` / `make.defaults`** sourcing (brush) — incremental
  merge of `USE_EXPAND`, `FEATURES`, `ACCEPT_*`. The brush `+=` array-append fix
  (`9086ca4`) and the `[[ -v assoc[key] ]]` fix (`aa172f9`) are in; confirm no
  regression in the incremental variable model.
- **`md5-cache` / `metadata` parse** (`portage-metadata`) — `auxdbkey_order`,
  `REQUIRED_USE` expr, `SRC_URI` tree. The computed-SRC_URI fix (`2965fa2`) is in;
  spot-check the cache-entry field set against `auxdbkey_order`.

## Method

For each: pick 3-5 representative inputs (including the `-*` and USE_EXPAND
edge cases), run both em's parser and portage's reference (`portage.config` /
`portage.dep` / `portage.cache.metadata`), and diff the resolved values.
`portage-repo/bench.sh`'s `compare_caches` example is the template for the cache
field comparison (semantic, order-independent).

Record divergences here as 🔴 items; the known-intentional ones (install-order,
flag ordering in display) are in `docs/architecture.md` § "Known divergences".

## Why now

These parsers feed the solver, the USE fold, the license/keyword gates, and the
fetch SRC_URI — i.e. everything the binhost/stage work leans on. A silent parse
regression there would mismatch `emerge -p` or mis-merge before the binpkg layer
can catch it.
