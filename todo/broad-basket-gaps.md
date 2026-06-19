# Two `-p` divergences found 2026-06-18 (broad basket sweep)

STATUS: **B fixed; A narrowed to a report-coverage sub-gap.** B (the
`dev-lang/python` over-pull) was root-caused and fixed — see
`todo/target-derivation.md`. A is no longer a *plan* divergence: re-checked
2026-06-19, `em -p sys-apps/systemd` and `emerge -p` now list the **same 30
packages** (the earlier 30-vs-33 gap reflected a since-changed box state). What
remains is purely **blocker-advisory coverage** (Tier-2 reporting), below.

## A. `sys-apps/systemd` — blocker-report coverage (plan AT PARITY)

Re-verified 2026-06-19: package set matches emerge (30 == 30). The residual gap
is that em's blocker advisory surfaces **2 of emerge's 4** `blocks B` edges; the
prose below documents the original 30-vs-33 state for context.

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

## B. `dev-lang/python` — over-pull → **diagnosed, moved**

Root-caused 2026-06-19 to the CLI target-derivation path (two bugs: the sibling
slot listed `[R]`, and slot/version qualifiers ignored entirely). Full diagnosis,
emerge reference table, and fix plan now live in
**`todo/target-derivation.md`**; intended behaviour is documented in
`docs/architecture.md` §"Target derivation: argv → request".
