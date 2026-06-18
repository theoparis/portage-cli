# Two `-p` divergences found 2026-06-18 (broad basket sweep)

STATUS: **open** (characterized, not fixed). After the broot USE-dep fix
(`todo/broot-filter-use-dep.md`, commit 5359c30) the full parity basket is
`RESULT: parity OK` on the standard targets. A wider sweep over 20 targets
found these two remaining diffs; both are categorised, one is expected, one is
new and needs a root-cause trace.

## A. `sys-apps/systemd` — 3 under-pulls (EXPECTED, Tier-2 blockers)

**Context:** the test box is a **systemd-less** profile
(`default/linux/arm64/23.0`, OpenRC base — no `systemd` USE, no systemd
subprofile). `sys-apps/systemd-utils-255.18` is installed and provides
`virtual/tmpfiles`/`virtual/udev` (the systemd-less split). systemd is **not
masked** in the profile — asking for it directly is user error on this box, but
both tools resolve it; the divergence is in how they treat the resulting soft
blocks.

`em -p sys-apps/systemd` lists 30 vs emerge's 33. The 3 missing are all
**soft-blocker** edges emerge resolves by *replacing* the installed package
(`blocks B`), which em reports but does not act on (architecture "Known
divergences", Tier-2):

- `net-dns/openresolv` — systemd RDEPEND `resolvconf? ( !net-dns/openresolv )`.
  em keeps openresolv installed (soft-block) and reports `blocks B`; emerge
  **uninstalls** it to satisfy the blocker. (openresolv is installed here.)
- `sys-apps/gentoo-systemd-integration` — PDEPEND `!vanilla? ( … )`, soft-blocks
  the installed `sys-apps/systemd-utils-255.18`.
- `sys-apps/systemd-initctl` — PDEPEND `!sysv-utils? ( … )`.

emerge's full block report:
```
[blocks B] sys-apps/systemd ("sys-apps/systemd" is soft blocking sys-apps/systemd-utils-255.18)
[blocks B] sys-apps/gentoo-systemd-integration ("…" is soft blocking sys-apps/systemd-utils-255.18)
[blocks B] net-dns/openresolv ("net-dns/openresolv" is soft blocking sys-apps/systemd-260.2-r1)
[blocks B] sys-apps/systemd[resolvconf] ("…" is soft blocking net-dns/openresolv-3.17.4)
```

This is the documented "blockers: reported, not used to exclude/replace"
divergence, surfaced concretely on the destructive case. **No action expected**
beyond confirming em emits the matching `blocks B` report; closing it would
require promoting blockers to exclusion/replacement (a known Tier-2 → Tier-1
promotion item, see architecture §"Known divergences").

### Blocker advisory coverage (verified 2026-06-18)
em **does** emit a blocker advisory for this plan:
```
!!! Blocker conflict(s) detected:
  sys-apps/systemd-utils:0-255.18 blocks !sys-apps/gentoo-systemd-integration (weak(!))
  sys-apps/systemd-utils:0-255.18 blocks !sys-apps/systemd (weak(!))
```
It covers 2 of emerge's 4 `blocks B` edges. **Missing:** the
`resolvconf? ( !net-dns/openresolv )` pair — the **USE-conditional** blocker.
em evaluates the blocker's USE condition but does not surface the blocker when
the gating flag (`resolvconf`) is on. That is the concrete sub-gap if we want
full blocker-report parity (separate from the Tier-2 replacement question).

## B. `dev-lang/python` — 1 over-pull (NEW, needs trace)

`em -p dev-lang/python` lists **two** python entries:
- `dev-lang/python-3.13.12  [R]`   ← spurious rebuild
- `dev-lang/python-3.14.6  [U] [3.14.5]`

`emerge -p dev-lang/python` lists only `python-3.14.6 [3.14.5]` (the target is
the newest slot, 3.14; 3.13/3.12 are untouched).

### What it is NOT
- NOT bare-name multi-slot targeting: `em -p dev-lang/python:3.14` (explicit
  slot atom) **still** rebuilds 3.13. And `target_package`
  (`portage-cli/src/query/depgraph/repo.rs:694-700`) already collapses a bare
  name to the single newest slot.
- NOT a version bump: 3.13 is `[R]` (same version 3.13.12, rebuild in place),
  not `[U]`.

### Open question (trace next)
Why is the **3.13 slot** rebuilt when only `:3.14` is targeted? Candidates:
1. A reverse dependency in the plan that `dev-lang/python:3.13` satisfies is
   being rebuilt, pulling python:3.13 as a build/runtime dep — but emerge does
   not, so check whether em is over-eagerly rebuilding something that pins 3.13.
2. The installed-slot rebuild path (`InstalledPolicy`) re-emits a kept older
   slot the target does not name. Compare with the `installed-revbump` Favor
   fix (`todo/installed-revbump-update-on-prune.md`): a non-target installed
   dep should be *kept*, not rebuilt.

### Repro
```bash
em -p dev-lang/python          # shows python-3.13.12 [R] AND python-3.14.6 [U]
em -p dev-lang/python:3.14     # STILL rebuilds 3.13 — so it's not name resolution
emerge -p dev-lang/python      # only python-3.14.6 [U 3.14.5]
```
Installed: python-3.12.13_p1 (3.12), 3.13.12 (3.13), 3.14.5 (3.14).
