# Stage-build shakeout (em --root @system, 2026-06-26)

First real `em toolchain --setup` → `em --root @system` into `/var/tmp/stage1-base`
on the 128-core box. Toolchain step: clean. `@system`: **148/163 merged**, 6
failures. The toolchain→@system sequence works (staging the toolchain first
clears the glibc↔gcc pre-flight cycle). Failure classes:

## FIXED — CBUILD unset → python configure "cross" (`50081f2`)

`dev-lang/python` died at configure: `Cross compiling required --host=HOST-TUPLE
and --build=ARCH`, with build==host==aarch64-unknown-linux-gnu. The host crossdev
`config.site` was a **red herring** (it gates on `CBUILD != CHOST`, a no-op when
CBUILD is unset). Real cause: em left **CBUILD unset**, so `econf` omits `--build`
(`${CBUILD:+--build=…}`), configure sees `--host` alone → `cross_compiling=maybe`
→ python's strict check dies. Portage defaults CBUILD to CHOST (`portageq envvar
CBUILD` = CHOST even with none in make.conf). Fixed: em sets `CBUILD=CHOST` when
unset (`shell.rs`). Verified: cpio's VDB env now has
`CBUILD="aarch64-unknown-linux-gnu"`.

## OPEN — `fowners` fails for root/other-user chowns (eselect, pam)

`die: fowners failed` in `src_install`. em's `fowners`
(`install.rs` `FownersCommand`) shells to the **host** `chown` with the owner
string verbatim. Two facets:

1. **Unprivileged chown (likely dominant).** The build runs as `lu_zero` under
   `~/.cache/em/build`; `chown root:shadow <file>` (pam's `unix_chkpwd`,
   eselect's files) → `EPERM` — a non-root user cannot chown to root. Portage
   handles this with `FEATURES=fakeroot`/userpriv handling (or a privileged
   merge). em has none, so any package that `fowners` to a foreign user fails.
   This will hit MANY packages, not just these two — it just happens these were
   the first in @system to fowners to root/other.
2. **Name resolution against the wrong root.** Even privileged, the owner is
   resolved against the **host** `/etc/passwd`+`/etc/group`, not the ROOT's
   (where `acct-user/*`/`acct-group/*` installed the ids). Portage resolves
   uid:gid from the *target* passwd/group then chowns numerically. A name absent
   on the host fails; a name with a different id on the host chowns wrong.

Fix direction: resolve owner→uid:gid against `${ROOT}` (or `${EROOT}`)
passwd/group, and do the chown under fakeroot semantics (record ownership in the
image without real privilege) — i.e. a fakeroot-equivalent for the install
phase. Bigger than a one-liner; ties into [[build-clean-env]] (privilege/sandbox
model). The minimal hand-built stage1 didn't hit it because its packages
(glibc/bash/coreutils) fowners little; @system breadth exposes it.

## Transient-looking but actually 3 fetcher bugs

`popt`, `tar`, `psmisc` "could not be fetched" — NOT flakiness, three distinct
bugs. See [[distfile-fetch-reliability]] (investigating next):
- **popt**: `error decoding response body` on the upstream URL, **no Gentoo
  mirror fallback**.
- **tar**: `HTTP 404` on `alpha.gnu.org`, then `fetch: … ok` on a fallback — yet
  the package was **still marked failed** (success-after-fallback not registered).
- **psmisc**: a **truncated 139431-byte** file (expected 432208) cached in
  DISTDIR, fails manifest verify forever — **corrupt partial not discarded/refetched**.

## Base state
`/var/tmp/stage1-base`: 148 pkgs incl. gcc/glibc/bash/make/sandbox. Missing
python/portage (python blocked the chain → now fixed) and the fowners/fetch
casualties. Re-run after fowners lands to get a complete self-extending base.
