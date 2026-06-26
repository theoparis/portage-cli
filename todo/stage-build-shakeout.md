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

## UPDATE 2026-06-26 — fixes landed, base at 160; the wall is privilege

After CBUILD (`50081f2`), fowners (`efdeb37`), and GENTOO_MIRRORS/make.globals
(`e0bae58`): re-ran `@system` into `/var/tmp/stage1-base` → **160 pkgs, python
built** (CBUILD validated end-to-end), pam/eselect/popt now merge. 3 of 70
remain, and they expose the boundary:

1. **util-linux — the fakeroot/privilege wall (blocks portage).** util-linux's
   *own* Makefile `install-exec-hook-mount` runs `chown root:root …/bin/mount`
   (setuid mount); unprivileged → `Operation not permitted`. This is **not** em's
   `fowners` (fixed) — it is the package's direct chown. portage RDEPENDs
   `sys-apps/util-linux`, so this blocks the self-extending base. A full `@system`
   stage with setuid binaries fundamentally needs **root or fakeroot**, exactly as
   catalyst runs stage builds as root. Options: (a) run `em` as root for stage
   builds (simplest, gives a real root-owned stage3); (b) integrate fakeroot
   (intercept/record chown unprivileged) — bigger, preserves the unprivileged
   model. The fowners fix only covers em's builtin; package-internal chowns need
   one of these. **This is the decision point for a real stage3.**
2. **bash — re-merge over a read-only file.** `copy image/usr/bin/bashbug →
   ROOT/usr/bin/bashbug: Permission denied`: the existing dest is mode 0555 (no
   write bit) and em's merge writes over it without `unlink`/chmod first. Portage
   unlinks before installing. Only bites on *re*-merge (a fresh root is fine).
   Clean fix: unlink (or chmod +w) the destination before overwriting.
3. **psmisc — fetch, two layered issues.** sourceforge returns a ~139 KB
   error/redirect page (not the tarball); the GENTOO_MIRRORS fallback now fires
   (the make.globals fix works) but builds the **flat** `distfiles/<file>` path,
   which 404s — modern mirrors use the **hashed** layout (`distfiles/<hash>/<file>`
   per the mirror `layout.conf`). See [[distfile-fetch-reliability]] — the mirror
   URL must honour the mirror layout, not assume flat.

Net: the unprivileged path reaches ~160/163; setuid/privileged packages
(util-linux) need root/fakeroot. For a real (root-owned) stage3, run `em` as root
— then `fowners` and Makefile chowns both work and the tree is properly owned.
