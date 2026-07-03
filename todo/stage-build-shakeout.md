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
2. ✅ **Name resolution against the wrong root** (FIXED — facet 2,
   `907d914`). `fowners` now resolves `user[:group]` to numeric uid:gid against
   the target `<ESYSROOT|EROOT>/etc/{passwd,group}` (gated on an offset root) and
   chowns numerically, mirroring portage's `__resolve_owner`; the faking is the
   fakeroost session [[fakeroot-privilege-backends]]. Was: owner resolved against
   the **host** db, so a name absent on the host failed or chowned wrong.

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

## 2026-07-03 — resumed under pseudoroot: util-linux clears, two real findings

Resumed the same `/var/tmp/stage1-base` root's `@system` (67 pkgs) with
`--privilege pseudoroot` (v0.2.1, shipped 2026-07-03) to check whether the
util-linux privilege wall above is actually cleared now. **It is** — `sys-apps/
util-linux-2.42.1` merged clean unprivileged, no chown failures. 64/67 merged on
that pass.

**Self-inflicted false alarm (process hygiene, not a bug).** Mid-run I rebuilt
`target/release/em` (`cargo build --release`) while the background `@system` run
was using that exact binary. `spawn_install_worker`/`reexec` resolve the child via
`std::env::current_exe()` fresh at spawn time (`privilege.rs`), so a worker that
happened to spawn while cargo's linker was mid-replace of the file hit `pseudoroot:
failed to execute supervised command: No such file or directory (os error 2)` —
looked exactly like a pseudoroot bug, wasn't. **Lesson: never `cargo build
--release` the same binary a background `em` run is currently using — even for
an unrelated change.** Wait for the run to finish, or build to a different path.

**Real finding #1 — acct-group/acct-user stale VDB entries predate pseudoroot,
not a live bug.** `sys-apps/shadow`'s `fowners root:shadow` died: `invalid group
in /var/tmp/stage1-base/etc/group: :shadow`. Root cause chain:
- `acct-group.eclass`/`acct-user.eclass` **are already ROOT-aware** — `pkg_preinst`
  calls the real `groupadd`/`useradd` with `--prefix "${ROOT}"` when `ROOT` is set.
  em needs no shim here; nothing to build.
- But that same `pkg_preinst` gates on `[[ ${EUID} -ne 0 || -n ${EPREFIX} ]]` →
  `einfo "Insufficient privileges…"; return` — a **silent no-op**, not a die, not
  a failure the merge sees.
- `acct-group/shadow` (and 20 sibling acct-group/acct-user pkgs) in this test root
  were merged **2026-06-26 17:01**, a full week before the pseudoroot backend
  existed (`37e8d49`, 2026-07-02) — so `EUID` was the real unprivileged uid, the
  gate fired, group/user creation was skipped, and the VDB recorded a normal
  successful merge anyway (correct behaviour for that gate at the time — just
  stale data in *this* long-lived test root, not a reproducible bug against
  current em).
- Confirmed the fix is "re-merge, not code": `em --emptytree acct-group/shadow`
  under current pseudoroot → `* Adding group shadow` → written into
  `<root>/etc/group` correctly. Batch-re-merged all 21 acct-group + 5 acct-user
  pkgs in the root the same way; 20/27 landed clean this way (see finding #2 for
  the one that didn't).
- **Takeaway for future long-lived test roots**: any acct-group/acct-user package
  merged before a privilege backend existed (or under `--privilege none`) needs
  re-merging once a real backend is in place — its "installed" VDB state lies
  about whether the group/user actually exists on disk.

**Real finding #2 — ROOT-CAUSED: a `brush` process-substitution fd-lifecycle bug,
NOT pseudoroot/acct-user-specific.** Re-merging `acct-user/portage-0-r4` (already
installed → an `--emptytree` self-replace: `pkg_prerm`→`pkg_postrm` for the old
copy, then `pkg_preinst`→register→`pkg_postinst` for the new) hung indefinitely —
12+ min, 0% CPU, all 128 tokio worker threads parked (`futex_do_wait`/`ep_poll`,
genuinely idle). `build.log` showed every phase through `>>> pkg_postinst`
*starting*, nothing after; VDB registration already happened
(`counter=326` printed) — so the hang is strictly inside the `postinst` phase's
own execution.

Traced with `/proc/<pid>/fd` (no `strace` on this box): the worker process
(`em __worker`, pid 76946 in the reproduction) held **two** file descriptors
(11 and 15) open on the same pipe, and that pipe's *read* end was the stdin of
an orphaned `tee -a build.log` child (confirmed: `readlink /proc/76946/fd/11`
== `readlink /proc/<tee-pid>/fd/0` target). `tee` was blocked on `read()`
forever because the pipe's write end was never fully closed — the worker itself
still held it open.

**The construct responsible**: `EbuildShell::run_phase`
(`portage-repo/src/build/shell.rs:1698-1705`) builds, for every non-quiet phase,
```
{ func_name ; } > >(cd / && tee -a {log}) 2>&1
```
and `await`s it via `run_string`. Two things compound here: (1) `2>&1` duplicates
the process-substitution pipe's write end onto a second fd (stdout *and* stderr
both point at it — matches the 2 fds observed), and (2) a comment already in that
code (`"The process-sub body may be polled after the phase (and even after the
build tree is cleaned up)"`) shows a past session already knew brush's `>(...)`
completion is lazy/asynchronous and only patched *one* symptom of that (the
substituted `tee` starting from a deleted `${S}`) via the `cd /` hack — not the
underlying fd-closing gap. Somewhere in brush's handling of this compound command,
the write-end duplicates aren't both closed once the phase function returns, so
`tee` never sees EOF; whatever `run_string` awaits internally to consider the
command "done" apparently can't complete while that dangling reference exists,
so `run_phase`'s `.await` — and the whole worker — hangs.

**This is a `brush` bug (`~/Sources/brush`), not portage-cli merge logic**, and
it's *latent*, not new: any non-quiet phase invocation exercises this exact
construct. It almost certainly hasn't hung visibly before because (a) most phases
finish before/without straining whatever race window causes brush to fail to
close both fds, and (b) even when it *does* leak, if nothing downstream needs to
wait synchronously on that phase's completion signal in the same way, the orphan
`tee` is just silently left running in the background (reparented to init) rather
than blocking `em` itself — i.e. **this session's earlier "successful" merges may
have left orphaned `tee` processes behind unnoticed**; only this specific replace
(more phases run back-to-back in one shell → more chances to hit the race, and/or
more postinst output volume) surfaced it as a visible hang. Not proven
pseudoroot-specific — no evidence yet it's backend-dependent at all, since the
construct runs in the unwrapped brush shell regardless of privilege backend.

**Fix directions (not done today)**: (a) fix brush's `>(...)` + `2>&1` fd
lifecycle upstream — needs Luca's go-ahead per [[dont-commit-to-sibling-repos]];
or (b) stop depending on brush process substitution for phase dual-logging
entirely — spawn `tee` as a plain `std::process::Child` with `Stdio::piped()`
that portage-cli owns directly (explicit writer-closes-then-`.wait()`), removing
the dependency on brush's `>(...)` semantics for something that's purely
cosmetic console+file duplication. (b) is probably the more robust fix since it's
self-contained in portage-cli. **Before landing either fix, check for orphaned
`tee -a build.log` processes accumulated from earlier @system runs on this box**
(`pgrep -fa 'tee -a'`) — they may be harmless zombies-in-waiting, but worth a
sweep.

**Status:** `stage1-base` @system resume is paused here — 20/27 acct pkgs fixed,
`acct-user/portage` blocked on this hang, `@system` itself hasn't been resumed
since. [[fakeroot-privilege-backends]]

## 2026-07-03 (later) — hang ROOT-CAUSED for real and FIXED: tokio LIFO-slot
## stranding, not an fd-lifecycle leak

The fd-lifecycle theory above was wrong. Minimal repro (hangs deterministically
under unpatched brush, no em involved):

```bash
echo "res: $( { read -r x; echo got-$x; } < <( echo hi ) )"
```

**Any read-side process substitution inside a command substitution deadlocks.**
The acct-user trigger is `egetgroups`' `while read …; done < <( printf … | sort )`
running inside `old_groups=$(egetgroups …)` in `pkg_postinst` — before any
output, matching the empty log after `>>> pkg_postinst`.

Mechanism (three ingredients, all in brush):
1. `setup_process_substitution` (`brush-core/src/interp.rs`) runs the `<(…)`
   body via `tokio::spawn` and returns without ever awaiting/yielding.
2. Command substitutions execute their body as a *spawned task*
   (`invoke_command_in_subshell_and_get_output`), so inside `$( … )` the procsub
   spawn happens **on a tokio worker** — and a fresh spawn from a worker lands in
   that worker's **LIFO slot, which other workers cannot steal**.
3. The parent then blocks the same worker thread in a synchronous `read(2)` on
   the procsub pipe (`SharedPipeReader::poll_read` does blocking I/O; the `read`
   builtin's async path goes through it) without returning to the scheduler
   loop. The body task never gets its first poll → EOF never comes → deadlock,
   no matter how many workers are idle.

Verified thread picture on a hung repro: 129 threads = 1 in `anon_pipe_read`
(the stuck worker), 1 in `ep_poll`, 127 parked in futex — identical to the em
hang above (the single `anon_pipe_read` thread went unnoticed among 128).
Top-level scripts don't hang because `block_on`'s main future is not a worker
task: its spawns go to the global inject queue, so any worker picks the body up
(this is also why the write-side `> >(tee)` construct only ever produced *late*
tees, never a visible hang — phases yield via external commands).

**Fix (in ~/Sources/brush working tree, UNCOMMITTED per repo policy):**
`setup_process_substitution` made async + `tokio::task::yield_now().await`
after the spawn. The yield forces one trip through the scheduler loop, which
polls the LIFO slot (body gets its first poll; from then on its wakeups are
reactor-driven and stealable) and re-queues the parent at the stealable end of
the run queue. Chosen over a oneshot started-handshake because a same-worker
wake would park the *parent* in the LIFO slot — yield can't strand either side.

Verified: minimal repros + 50× egetgroups replay pass; brush compat suite
2240 cases 0 failed (one PTY job-control test flaky in full-suite runs,
pre-existing, passes in isolation with and without the patch); end-to-end
`em --root /var/tmp/stage1-base --config-root / --privilege pseudoroot
--emptytree -1 acct-user/portage` — the exact hang — merges clean in seconds,
`pkg_postinst` runs `usermod` ("Updating user portage"). Orphan-tee sweep:
none found. Remaining: Luca to review/commit the brush patch, push, bump the
`Cargo.toml` rev pin; then resume `@system`.

**`@system` resumed and DONE (2026-07-03).** Rebuilt the release binary against
the patched `for-portage-repo` worktree (`9baec193`), re-ran
`em --root /var/tmp/stage1-base --config-root / --privilege pseudoroot
--keep-going @system` for the remaining packages: **50/50 merged, 0 failures**,
no hangs, no orphaned `tee` processes afterward. The native
toolchain→stage1-base→`@system` pipeline under pseudoroot is now clean start to
finish on this box. Next real step is the actual `em stages --stage1` /
`packages.build` production path (see [[em-stages-and-binhosts]]) rather than
the ad-hoc full-`@system` proxy this shakeout has used throughout.

## 2026-07-03 — first cross-stage1 attempt: three from-scratch gaps found + fixed

Tried a genuinely fresh cross-stage1 (`em crossdev -t riscv64-unknown-linux-gnu
--setup --root /var/tmp/cross-stage1-riscv64 --privilege pseudoroot`, no reuse of
`~/.gentoo`) — the self-contained `--root` crossdev path (own empty VDB, no
host-shared libs) described in `todo/crossdev-target.md`'s design table had never
actually been exercised end-to-end. It wasn't ready:

1. **No `repos.conf`/no `gentoo` main-repo entry for a bare `--root` EPREFIX.**
   `--root DIR` retargets `config` (not just `base`/`target`) away from the host
   — unlike `--prefix`, which only offsets the install target and keeps config
   shared. `em crossdev --init-target`'s `main_repo()` only ever looked at the
   *target's own* `repos.conf`, which is empty on a truly fresh root, so it
   failed immediately with "no main repo configured". Fixed: `main_repo()` now
   falls back to the host's `repos.conf`, then to the hardcoded
   `/var/db/repos/gentoo` default (mirroring `Cli::repo_path`'s existing
   fallback). `ensure_repos_conf` now also writes a `gentoo.conf` entry into the
   EPREFIX's own `repos.conf` (not just the crossdev overlay) whenever
   `roots.config()` is `Some` (i.e. genuinely retargeted, not host-shared) — so
   subsequent builds resolve without needing a `--config-root /` workaround.
2. **No `make.profile` for a bare `--root` EPREFIX.** Same root cause: no host
   config sharing means no profile either. Fixed: `ensure_prefix_profile` links
   the EPREFIX's `make.profile` to the *host's* resolved profile (canonicalizing
   `/etc/portage/make.profile`) — the EPREFIX builds host-arch packages
   (the crossdev toolchain lands on `ROOT=/`-equivalent), so unlike the target
   sysroot (which links the *target* arch profile), it needs the host's own.
   No-op for `--local`/`--prefix` (already host-shared).
3. **Cross binutils kept `debuginfod` unconditionally**, assuming the cross
   EPREFIX is always host-rooted (deps pre-satisfied). A self-contained `--root`
   EPREFIX is exactly as empty as native's, so it hit the same explosion native
   already avoids (elfutils → curl → c-ares/nghttp2/nghttp3 → …, dozens of extra
   packages) — and needs the same missing bare-FS `baselayout` skeleton step
   native has, for the same `/usr/lib/../lib64` osdir reason. Fixed:
   `toolchain_plan` takes a new `self_contained: bool`; when true (a bare
   `--root` EPREFIX, detected via `roots.config().is_some()`), `Cross` now gets
   the same `baselayout` step + `-debuginfod` binutils USE that `Native` always
   had. Existing host-shared behaviour (`self_contained = false`) unchanged.

**Also observed, resolved as a side effect**: `die: ERROR: 23.0 merged-usr
profile, but disk is split-usr` (from `profiles/releases/23.0/profile.bashrc`)
fired repeatedly during the *first* (broken) attempt, once per package — but
did **not** actually stop the run; packages kept registering right after each
one. A 4th bug was found landing the fixes above: the `baselayout` StageStep
was still being cross-rewritten to `cross-<tuple>/baselayout` by `atom()`
(which unconditionally rewrites every component for `BootstrapKind::Cross`) —
but baselayout isn't part of the cross overlay's package set at all (only the
toolchain components are symlinked there), so it failed outright with "no
ebuilds found". Fixed: baselayout now always uses the literal
`sys-apps/baselayout` atom, bypassing the cross rewrite, for both `Native` and
self-contained `Cross`. Once baselayout actually ran (creating the real
`bin -> usr/bin` etc. merged-usr symlinks), the "merged-usr" die disappeared
entirely (0 occurrences on the clean re-run) — so it was never a separate
die-flag-propagation bug, just a faithful symptom of the missing skeleton.

**Status (2026-07-03)**: the four plumbing gaps above (repos.conf/profile/
baselayout-category/debuginfod) are fixed and verified — the retry got past all
of them cleanly (baselayout, binutils, os-headers, kernel-headers, libc-headers
all merged) and reached real compilation: `[6/8] gcc-stage1` (cross-riscv64
gcc-15.2.1's host-side build).

**5th finding — OPEN, architectural, not yet fixed.** `gcc-stage1`'s plan (11
packages) pulled in a full **`sys-libs/glibc-2.43-r2`** (host-arch, non-headers,
NOT the `cross-riscv64-…/glibc` already built for the target) — correctly: the
cross compiler binaries (`riscv64-unknown-linux-gnu-gcc`) are themselves
HOST-ARCH executables that need a working HOST libc to link against, and the
self-contained EPREFIX had none. The solver did the right thing; the EPREFIX
just doesn't have what it needs yet. The build then failed compiling
`libiberty/obstack.c`:
```
error: request for member 'extra' in something not a structure or union
error: unknown type name '_OBSTACK_SIZE_T'
```
— an `obstack.h` struct-layout mismatch. The actual compile command shows
`-I/var/tmp/cross-stage1-riscv64/usr/include` (the EPREFIX's own, just-built
glibc headers) listed **before** `-I…/libiberty/../include` (gcc's own bundled,
version-matched `obstack.h`), so the compiler picks up the freshly-built
glibc's copy instead of gcc's own — the two aren't ABI-compatible at this
combination of versions/build state.

Tried hypothesis 1 first (build a full native aarch64 toolchain into the same
EPREFIX before layering crossdev on top): `em toolchain --setup --root
/var/tmp/cross-stage1-riscv64 --privilege pseudoroot` ran baselayout→binutils→
kernel-headers→**full native glibc** cleanly, then hit the **exact same**
`libiberty/obstack.c` failure building plain `sys-devel/gcc` — **not** a cross
package, and with a toolchain that had *just* successfully built the glibc it
was choking on. That rules hypothesis 1 out completely: it was never about
needing a toolchain first.

**Root-caused (6th finding): `setup::bootstrap`'s own `--root`-mode bashrc was
the bug, and it's a regression from *this session's* earlier fix (finding 1
above).** Before finding 1, native `toolchain()`/crossdev `init_target()`
never called `setup::bootstrap` at all for a self-contained root (that's
*why* it needed the repos.conf/profile fix in the first place) — so no bashrc
file existed, and none of this ever fired. Adding `ensure_self_contained_prefix`
(which calls `setup::bootstrap`) fixed repos.conf/profile but, as a side
effect, *also* started writing `BASHRC_PREFIX` — which unconditionally exports
`CPPFLAGS="-I${ROOT}/usr/include …"` whenever `$ROOT` is set and non-`/`,
**with no distinction between "a `--prefix DIR` layered on a shared host base"
(what it was designed for — the host's own headers are already found by
normal search, so the prefix needs an explicit assist) and "a self-contained
`--root DIR` that IS the whole system"** (no such gap — SYSROOT/CHOST wiring
already resolves everything through the compiler's normal/cross search order).
For the self-contained case this extra `-I<ROOT>/usr/include` doesn't just do
nothing: it lands ahead of a package's own project-local `-I` flags (gcc's
`libiberty/../include`) and shadows the version-matched local `obstack.h` with
the ROOT's own ABI-mismatched one from its just-built glibc.

**Fixed**: `setup::bootstrap` now checks `roots.build_sysroot()` — `None` means
base == target (a genuine self-contained `--root`, no separate host base to
layer over) — and writes an **empty** bashrc there instead of `BASHRC_PREFIX`.
`--prefix DIR` (`build_sysroot()` is `Some`) and `--local` (its own
`BASHRC_LOCAL`) are unaffected. Two new tests in `setup.rs` lock in both sides.

**7th finding: no `MAKEOPTS` at all for a self-contained `--root`, so every
build defaulted to serial.** Retried with the bashrc fix — gcc's own build got
past `obstack.c` cleanly this time, but "taking way too long" turned out to be
real: `ps aux` showed a single `cc1plus` at a time on this 128-core box, over
an hour into gcc's full multi-stage bootstrap. Cause: `make_conf_template`
writes a purely commented placeholder (`# Profile and base make.conf come from
the host…`) — true for `--local`/`--prefix` (which share the host's real
make.conf, `MAKEOPTS="-j80 -l80"` on this box), but **false** for a
self-contained `--root`: its own `etc/portage/make.conf` is the *only* one
ever read, and it had no `MAKEOPTS` line at all, so every build (baselayout,
binutils, glibc, and this gcc bootstrap) had been running effectively `-j1`
the whole time. **Fixed**: `make_conf_template` takes the same
`self_contained` flag as the bashrc fix and, when true, writes a real
`MAKEOPTS` — the host's own value if readable (`MakeConf::load_default`),
else `-j<nproc>`. `--local`/`--prefix` keep the pure-comment template
(unaffected — they already inherit the host's real `MAKEOPTS`). Two new tests
lock this in.

**Native toolchain bootstrap CONFIRMED working from scratch (2026-07-03).**
With MAKEOPTS fixed, `em toolchain --setup --root /var/tmp/cross-stage1-riscv64
--privilege pseudoroot` ran all 5 steps clean (23 packages), and the resulting
`aarch64-unknown-linux-gnu-gcc` compiles and links a working executable
(verified directly). This is the first time a fully self-contained native
toolchain has been built from an empty `--root` end to end.

**8th finding: cross host-tool `ESYSROOT` only ever accounted for `--local`'s
`EPREFIX`, silently collapsing to the bare host path for a plain `--root`.**
Layering `em crossdev --setup --root <same dir>` on top of the now-working
native toolchain got through binutils/os-headers/kernel-headers/libc-headers
cleanly, then `gcc-stage1`'s own `libgcc` configure died: `cannot compute
suffix of object files: cannot compile`. The actual `./configure` invocation
showed `--with-sysroot=/usr/riscv64-unknown-linux-gnu` — a bare **host**
path, not `<our-root>/usr/riscv64-unknown-linux-gnu`. Traced to
`shell.rs::set_build_roots`'s cross-host-tool `ESYSROOT` special-case (from
the 2026-06-25 `~/.gentoo` cross bootstrap): `esysroot =
format!("{eprefix}/usr/{tuple}/")` — built from `eprefix` alone. `eprefix` is
only ever set for `--local` (Gentoo-Prefix); a plain `--root DIR` sets `ROOT`,
not `EPREFIX`, so `eprefix` was empty here and the whole expression collapsed
to the bare `/usr/<tuple>/` — the *host's own*, unrelated real crossdev
sysroot (which happens to exist on this box), not our test root's. The
build-tree `xgcc` then looked for target CRT/headers there instead of
`<our-root>/usr/riscv64-unknown-linux-gnu`, and libgcc's configure probe
couldn't compile.

**Fixed**: the cross-host-tool branch now builds from `root_str` (== `EROOT`,
already computed a few lines above and set to `ROOT+EPREFIX` universally) 
instead of bare `eprefix`. For `--local`, `root_str` == the eprefix path
already (identical result, no behaviour change — the 2026-06-25 fix stays
intact). For a plain `--root DIR`, `root_str` is the actual offset root, so
`ESYSROOT` now correctly resolves to `<DIR>/usr/<tuple>/`. No unit test added
(this function has no existing test scaffolding to extend — would need a full
synthetic `EbuildShell` + cross-category package fixture); validating via the
live cross bootstrap re-run instead.

**9th finding: the ESYSROOT fix (#8) was correct but incomplete — it doesn't
reach the actual failure, because `toolchain.eclass` computes its own
`PREFIX` from `EPREFIX` directly, bypassing ESYSROOT entirely for the cross
build path.** Re-ran with the ESYSROOT fix; `gcc-stage1`'s libgcc configure
failed at the *exact same point*, but the actual `--with-sysroot=` value was
now proven to come from a **different** eclass computation than the one
`ESYSROOT` feeds. Root cause, traced in `/var/db/repos/gentoo/eclass/
toolchain.eclass`:
- Line 274: `PREFIX=${TOOLCHAIN_PREFIX:-${EPREFIX}/usr}` — a top-level eclass
  variable, computed straight from the real `EPREFIX` env var (NOT ESYSROOT).
- For the `is_crosscompile` branch (which fires for `cross-<tuple>/gcc`),
  `--with-sysroot="${PREFIX}"/${CTARGET}` uses this `PREFIX`, and — this is
  the key structural fact — **the cross branch never emits
  `--with-build-sysroot` at all** (that flag only exists in the native/
  `else` branch, gated on `${ESYSROOT}`). So for a cross package there is no
  eclass-provided path back to ESYSROOT whatsoever; fixing ESYSROOT alone
  can't have touched this.
- On a **real, unprefixed Gentoo host** this is fine: `EPREFIX=""` →
  `PREFIX=/usr`, and `--with-sysroot=/usr/<CTARGET>` is *correct* because
  the whole crossdev bootstrap — kernel-headers, libc-headers, eventually
  gcc itself — installs everything to that same bare, unoffset path on the
  same real root. The freshly-built, not-yet-installed `xgcc`, invoked
  directly from its own build tree (not through a chroot) during its own
  `libgcc` configure, finds real content there because earlier steps put it
  there, unoffset, on the same filesystem.
- Our self-contained `--root DIR` breaks that assumption: earlier steps (
  linux-headers, libc-headers) installed into `<DIR>/usr/<CTARGET>` (correctly
  offset via `ROOT`), but gcc's own internal build computes its baked-in
  sysroot path from `EPREFIX` (empty, since only `--local` sets it) — so it
  looks at bare `/usr/<CTARGET>` on the host filesystem instead, which either
  doesn't exist or (worse, as here) is the *host's own separate, unrelated*
  real crossdev sysroot.

**Fixed, more substantially this time**: rather than patch this one flag,
`run_phase` (`shell.rs`) now treats a `cross-<tuple>/{binutils,gcc,gdb,
clang-crossdev-wrappers}` build as EPREFIX-style *regardless of `--local`* —
when `eprefix` is otherwise empty, it's set to `root_str` (and `ROOT`
correspondingly to `/`), mirroring exactly what `--local` already does for
every package. This is deliberately NOT a narrow flag patch: `EPREFIX` back-
feeds `PREFIX`/`--prefix`/`--with-sysroot` inside the eclass, AND determines
`ED` (`= D + EPREFIX`) — and DESTDIR+prefix is a *physical* install-path
convention (`make install DESTDIR=${D}` really writes under
`${D}${prefix}/...`), so whatever the eclass bakes into `--prefix` must also
be what our own merge step looks for inside the image. Flipping only
`ESYSROOT` (a pure DEPEND-resolution hint) could never have fixed this;
`EPREFIX`/`ROOT`/`ED` needed to move together, reusing the *already-correct*,
already-tested EPREFIX-subtree merge logic (`ebuild.rs::ed_image_dir`)
generically instead of inventing a new merge path for this one package class.

**Why this doesn't reopen the ESYSROOT/SYSROOT-doubling trap the #8 comment
warned about**: SYSROOT already equals `root_str` for a plain `--root` build
(unlike `--local`, where it's host `/`) — which is *already correct* for a
self-contained host toolchain, since it must link against the root's own
just-built native libc, not the real host's. ESYSROOT for this package class
is computed straight from `root_str`, independent of `eprefix` — so flipping
`eprefix` for `EPREFIX`/`ROOT`/`ED` does not double-count anything there.

**Left a structural note in the code** (`shell.rs`, right above the flip) for
next time this function needs touching: it derives six PMS location variables
(`ROOT`, `EPREFIX`, `ED`, `EROOT`, `SYSROOT`, `ESYSROOT`) through a chain of
locals, with two independent package-class special-cases (this one, and the
ESYSROOT one) that used to re-derive the same `category`/`pn` filter twice —
now unified into one `cross_host_tool_tuple`. If a third special-case ever
shows up, that function is worth extracting into a small `RootVars` value
type built by one function from `(category, pn, root_str, build_sysroot,
build_eprefix)`, so the cross-variable invariants (ED must track EPREFIX;
ESYSROOT must not double-count a flipped EPREFIX) are enforced in one place
instead of by convention scattered across a ~150-line function.

No unit test added for either shell.rs fix (#8 or #9) — this function has no
existing test scaffolding to extend (would need a full synthetic
`EbuildShell` + cross-category package fixture, a non-trivial harness this
file doesn't have precedent for). Validating both via the live from-scratch
cross bootstrap re-run instead; if this area gets touched again, building that
fixture is worth doing then rather than continuing to rely solely on live
runs.

**gcc-stage1 confirmed fixed**: re-ran with the EPREFIX/ROOT/ED fix —
`libgcc`'s configure now compiles successfully, and the plan advanced cleanly
to `[7/8] libc` (full glibc, built with the freshly-working stage1 compiler).

**10th finding: `<root>/usr/bin` was never on the build `PATH` for a
self-contained `--root`, so any package doing a live PATH-based tool lookup
for something this same root already installed silently failed.** `glibc`'s
own `pkg_setup` sanity check died: `linux-headers version too low!`, reporting
`(0.0.0 >= 3.2.0)`. `sys-libs/glibc`'s `get_kheader_version()` runs
`$(tc-getCPP ${CTARGET})` — a live PATH lookup for `riscv64-unknown-linux-gnu-
cpp` — and pipes a tiny `#include <linux/version.h>` probe through it. The
wrapper was verified to exist and resolve correctly on disk
(`<root>/usr/bin/riscv64-unknown-linux-gnu-cpp` →
`<root>/usr/aarch64-unknown-linux-gnu/riscv64-unknown-linux-gnu/gcc-bin/15/…`,
correctly `em select`-activated after the gcc-stage1 step) — the problem was
purely that `<root>/usr/bin` was never on `PATH` at all for this build, so the
lookup found nothing and `get_kheader_version` silently returned empty
(`tail -n 1` of no output), read as version `0.0.0` — not a missing-file
error, a *wrong-answer* one.

Why nothing else needed this until now: `--local`'s `BASHRC_LOCAL` already
adds `<EPREFIX>/usr/bin` to `PATH` (sourced per-phase from the config
overlay); the existing "cross-CC auto-export" `PATH` prepend
(`shell.rs::run_phase`, a few lines above the EPREFIX flip) only fires when
`CHOST != CBUILD` — which never happens for this whole staged bootstrap,
since the "cross" in a `cross-<tuple>/*` build lives entirely in `CTARGET`
(parsed by `toolchain.eclass`), not in `CHOST`/`CBUILD` (both stay the host
arch throughout `em crossdev --setup`/`em toolchain --setup`). Every earlier
step's own tool invocations were either absolute-path (gcc's own `-B` flags,
baked in at configure time) or didn't need a *live* PATH search for a
same-root tool at all — glibc's `tc-getCPP` is the first one that does.

**Fixed**: `run_phase` now unconditionally prepends `<root>/usr/bin` to
`PATH` when self-contained (`build_eprefix` and `build_sysroot` both `None`,
`root_str != "/"` — the identical `self_contained` condition used by the
`setup.rs` bashrc/make.conf fixes, finding #6/#7). Deliberately scoped to
self-contained only, not plain `--prefix`: a `--prefix` build already shares
a working host PATH, and unconditionally preferring the prefix's own
`usr/bin` there would be a new preference-order change with no reported gap
motivating it. Verified: `riscv64-unknown-linux-gnu-cpp` is now found and
`get_kheader_version` reads the real `6.18` from the just-installed headers.

No unit test added (same reasoning as #8/#9 — no existing `EbuildShell` test
fixture for a self-contained cross build; a stray full-suite run showed 11
transient failures on the first `cargo test -p portage-repo`, reproduced clean
on immediate retry — pre-existing parallel-test flakiness unrelated to this
change, most likely from `run_phase`'s process-global
`std::env::set_current_dir` racing across parallel test threads; not
chased further, but worth remembering if `portage-repo`'s test suite flakes
again).

**FULL CROSS TOOLCHAIN BOOTSTRAP COMPLETE, FROM SCRATCH, VERIFIED (2026-07-03).**
With the PATH fix, the retry sailed through glibc and `[8/8] gcc-stage2`
(the final, full compiler) and reported `>>> cross toolchain
riscv64-unknown-linux-gnu ready in /var/tmp/cross-stage1-riscv64/usr/
riscv64-unknown-linux-gnu`. Verified directly, not just trusting the exit
code: `riscv64-unknown-linux-gnu-gcc`/`-g++` both compile **and link** real
RISC-V executables (`file` confirms `ELF 64-bit LSB pie executable, UCB
RISC-V, RVC, double-float ABI`) for a plain C program and a C++ one
(`#include <iostream>`, exercising libstdc++ too). This is the first time a
*genuinely self-contained* cross toolchain — native host toolchain bootstrapped
first, cross toolchain layered on top, zero host-state sharing — has been
built end-to-end with `em`, on this or any prior session.

**Ten from-scratch gaps found and fixed this session**, all in service of this
one result: (1) no `repos.conf`/no `gentoo` main-repo entry for a bare `--root`
EPREFIX; (2) no `make.profile` for a bare `--root` EPREFIX; (3) cross binutils
unconditionally kept `debuginfod`, exploding the dependency closure; (4)
`baselayout` was wrongly cross-rewritten to a nonexistent overlay atom; (5)
missing `virtual/os-headers` merge for the EPREFIX itself; (6) `BASHRC_PREFIX`'s
CPPFLAGS injection broke self-contained builds (a regression introduced by
fix #1, within this same session); (7) no `MAKEOPTS` at all for a self-contained
root (silently serial builds); (8)+(9) cross-host-tool `ESYSROOT`/`EPREFIX`/
`ROOT`/`ED` only ever accounted for `--local`, not plain `--root`; (10)
`<root>/usr/bin` never on `PATH` for a self-contained root. All ten are
documented above with root cause, fix, and reasoning; #6 is a cautionary tale
worth remembering — a fix in this exact area introduced a regression that
took a second full cycle to catch, so changes here need the live re-test, not
just "it compiles."

**Cross-stage1 attempted for the first time — the `--cross` composition
"just worked" (2026-07-03).** With the toolchain solid, tried `em --root
/var/tmp/cross-stage1-riscv64 --cross riscv64-unknown-linux-gnu stages
--stage1 -p` — **zero new code needed**: `--cross`'s existing root-model
composition (config==base==target==the sysroot) plus `em stages --stage1`
(built earlier this session for the *native* case) combined correctly on the
first try. The dry-run plan is clean: `Root-aware cross plan: CHOST=
riscv64-unknown-linux-gnu CBUILD=aarch64-unknown-linux-gnu`, baselayout then
the riscv profile's own `packages.build` (~67 packages, `USE="-* build"`),
everything targeting `/var/tmp/cross-stage1-riscv64/usr/
riscv64-unknown-linux-gnu/` (the target sysroot, not the host or the EPREFIX).
This confirms the hypothesis from the start of this session's cross-stage1
work: the missing piece was never the CLI/plan composition (`em stages
--stage1` + `--cross`), it was the ten toolchain-bootstrap gaps above — once
a real toolchain exists in the root, cross-stage1 falls out for free.

**11th finding (a pre-existing bug, not part of the ten above): `packages`
removal never handled the `-*cat/pkg` form.** Resolving the riscv profile's
`packages` file (needed for `stage1_packages`'s version-qualification step)
hit `error: atom parse error: invalid dep: *sys-apps/busybox` — the very
first profile stack this codebase had ever loaded that uses this removal
syntax (`profiles/arch/riscv/packages` has `-*sys-apps/busybox`, removing
`default/linux`'s `*sys-apps/busybox` system add). `Profile::packages_raw`
(`portage-repo/src/repo/profile.rs`) stripped only the leading `-` before
parsing, leaving a bare `*` that `Dep::parse` doesn't understand. Per PMS, a
removal line echoes the *original* text of the addition it cancels (`*`
marker and all) — the marker doesn't change what gets removed (`Remove`
matches by dep identity regardless of whether the retained entry was
System or Plain). Fixed: strip an optional `*` after the `-` too. One new
test (`packages_removal_echoes_the_star_marker_of_the_add_it_cancels`).

**12th finding: `-j`/`-l`/`--keep-going` weren't `global = true`, so they were
rejected after any subcommand** — the exact same class of bug already fixed
once for `-p`/`-a`/`-D` (see `todo/crossdev-target.md`'s Stage-C notes:
"Also fixed: `-p`/`-a`/`-D` were not `global = true` in clap"), just never
hit for these three since nobody had tried `em stages --stage1 -j N
--keep-going` before. The execution path already threads them correctly —
`run_staged` (used by `stages`/`crossdev`/`toolchain`) calls the *same*
`emerge_atoms`/`emerge_atoms_inner`/`run_merge_plan` chain the default
no-subcommand flow uses, which already reads `cli.jobs`/`cli.keep_going` —
this is purely a clap argument-*position* gap (these flags work fine placed
*before* the subcommand name, e.g. `em -j 80 --keep-going stages --stage1`;
only *after* the subcommand name do they need `global = true` to parse).

**Tried `global = true` as the fix, reverted.** Marked `jobs`/`load_average`/
`keep_going`/`autounmask_write` `global = true` (matching the existing
`-p`/`-a`/`-D`/`--root`/`--cross` precedent) — but this is inconsistent with
how the *other* merge-behavior flags (`autounmask`, `autosolve_use`,
`buildpkg`, `usepkg`, …) are handled, and scatters `global = true` across many
individual fields on the monolithic `Cli` struct rather than grouping them.
Reverted per direction: these belong in a shared mixin struct (matching how
`DepgraphFlags` is already flattened into `ToolchainArgs`/`CrossdevArgs`/
`StagesArgs`), not sprinkled as individual global flags — a proper fix needs
to decide where that mixin lives and how `run_staged`/`emerge_atoms_inner`
read from it, which is real design work, not a one-line change. **Deferred**:
for now, place merge-behavior flags (`-j`, `-l`, `--keep-going`,
`--autounmask`, `--autounmask-write`, `--autosolve-use`, …) *before* the
subcommand name — that already works correctly today, no code change needed
for that ordering.

**Status**: the full cross-stage1 *plan* is now proven correct end-to-end,
for the first time, and `-j 80`/`--keep-going`/`--autosolve-use`/
`--autounmask-write` all work correctly when placed before the subcommand.
`--autosolve-use` correctly resolved the REQUIRED_USE conflicts (curl needing
`ssl`, util-linux's `su`↔`pam`, a cascading `ngtcp2[gnutls]` need).

**Blocked on a pre-existing, already-documented gap, not a new one**:
running the real build now hits exactly what `todo/PENDING.md` already
flagged — "packages.build DEPEND-into-ROOT residuals: `acct-group/root`,
`sys-fs/e2fsprogs`, util-linux ordering — re-test now that the DEPEND-trim
sysroot fix landed" ([[em-root-characterization]]). The pre-flight dependency
check reports `sys-apps/util-linux` needs `acct-group/root` and
`app-arch/libarchive` needs `sys-fs/e2fsprogs[…]` — neither present in the
resolved closure for this `--cross` target sysroot. This is the first time
that pre-existing gap has been reproduced at real scale (a full ~65-package
stage1 closure, not a single leaf package) — confirms it's still open, but
it's a distinct, pre-existing body of work (cross/ROOT-offset dependency-
closure correctness) from this session's ten toolchain-bootstrap fixes above,
not something to blindly extend into.

**12th finding, resolved: `MergeFlags` mixin, following the `DepgraphFlags`
precedent exactly.** Added `portage-cli/src/cli/merge_flags.rs`
(`#[derive(clap::Args)]`, 21 fields: `update`, `autounmask_write`, `oneshot`,
`fetchonly`, `buildpkg`, `usepkg`, `usepkgonly`, `getbinpkg`,
`getbinpkgonly`, `emptytree`, `tree`, `json`, `onlydeps`, `noreplace`,
`jobs`, `load_average`, `keep_going`, `autounmask`, `autosolve_use`,
`complete_graph`, `with_bdeps`, `exclude`), flattened both at the top-level
`Cli` (bare `em <atoms>` path) and into `ToolchainArgs`/`CrossdevArgs`/
`StagesArgs`. `crossdev/mod.rs` gained `merge_merge_flags(globals, args) ->
MergeFlags` mirroring the existing `merge_depgraph_flags` (bool fields OR'd,
`Option<T>` fields `.or()`'d, args wins), and `merge_depgraph_flags` itself
was generalized to take `&DepgraphFlags` instead of `&CrossdevArgs` so all
three call sites (`setup`/`toolchain`/`stage1`) can share it — `toolchain()`
and `stage1()` previously passed `args.depgraph_flags.clone()` straight
through with **no merge with the global copy at all**, a second latent
instance of the same position bug, fixed alongside this. `EmergeOpts` gained
a `merge_flags: Option<MergeFlags>` field (same override/fallback shape as
`depgraph_flags`), threaded through `emerge_atoms` → `emerge_atoms_inner`,
which now resolves `let merge_flags = merge_flags_override.as_ref()
.unwrap_or(&cli.merge_flags)` and reads every merge-behavior value off that
instead of `cli.X` directly.

**Important correction made mid-implementation**: initially wired
`equery depgraph`'s handler to read `globals.merge_flags.{emptytree,
autounmask_write, onlydeps, with_bdeps}` — i.e. reached into the full
merge-behavior mixin from a query-only command. Caught: `query::depgraph::
DepgraphOpts` (what that command actually calls) only ever consumes 7 of
the 21 `MergeFlags` fields (`empty`, `autounmask_write`, `autosolve_use`,
`onlydeps`, `with_bdeps`, plus `deep`/`nodeps` from elsewhere) — the other
14 (`buildpkg`, `usepkg`, `jobs`, `keep_going`, …) are meaningless for a
command that only resolves and prints, never merges. The `Depgraph` query
variant already had its own precedent for this: it declared a *local*
`autosolve_use` field and OR'd it with the global one
(`*autosolve_use || globals.autosolve_use`) rather than relying solely on
the global — one bespoke field per thing actually used, not a blanket
mixin. Fixed by giving the `Depgraph` variant its own local `emptytree`/
`onlydeps`/`with_bdeps` fields, each OR'd with the matching
`globals.merge_flags` field the same way `autosolve_use` already was.
Lesson for future mixin work: a mixin belongs on a consumer only if that
consumer actually reads most of its fields — a display-only command that
needs a handful of resolve-level knobs should declare exactly those, not
flatten the merge-behavior grab-bag "for convenience".

**Second correction, same review pass**: `autounmask_write` was in that
group too, and shouldn't have been — checked `query::depgraph::depgraph`'s
body (`portage-cli/src/query/depgraph/mod.rs:698,720`) and confirmed
`autounmask_write` genuinely writes `package.use`/mask/keyword fixes to
`<config_root>/etc/portage`, even when the caller is `equery depgraph`. A
read-only query command mutating host config as a side effect of `--help`-
adjacent flag is exactly the kind of thing that bites someone later (typo a
flag on a "just show me" command, get a surprise `/etc/portage` write).
Fixed: dropped `autounmask_write` from the `Depgraph` variant entirely and
hardcoded `autounmask_write: false` in its `DepgraphOpts` construction —
it still *reports* autounmask candidates (that path is unconditional,
independent of the flag), it just never persists them. The write-capable
`--autounmask-write` stays exposed only where a merge can actually follow
(bare `em`, `toolchain`, `crossdev`, `stages`). General principle for the
next mixin pass: check not just "does this consumer read the field" but
"does this consumer *write anything to disk*, and should a command of this
kind be allowed to."

Verified: `em -j 8 --keep-going stages --stage1 -p --root <dir>` and
`em stages --stage1 -j 8 --keep-going -p --root <dir>` now behave
identically regardless of flag position (both parse and reach the same
later failure/success point) — the bug that started this whole mixin
detour. Full workspace build + `cargo test -p portage-cli` (120 passed) +
`cargo test -p portage-repo` (124 passed + doctests) + `cargo fmt --check`
+ `cargo clippy --all-targets` all clean after the change.

**13th finding, resolved: `acct-group/root`/`sys-fs/e2fsprogs` missing from
the `--cross` stage1 plan — `--root-deps=rdeps` was permanently "on" for any
cross-arch target, when it should only apply during toolchain bootstrap.**
`sys-apps/util-linux`'s `DEPEND="${RDEPEND} virtual/os-headers
acct-group/root"` puts these two atoms in DEPEND *only* (not RDEPEND). Real
crossdev's `--root-deps=rdeps` (a documented work-around for the crossdev
bootstrap cycle: a still-empty target sysroot can't yet satisfy plain DEPEND
while its own toolchain is being built into it) makes the solver drop
DEPEND-only requirements for target-merge entries, keeping only RDEPEND.
`root_aware::detect()`/`CrossContext::root_deps_rdeps(host_arch)` derived
this purely from the sysroot's own `CHOST`/`CBUILD` (`--cross` sets
`CBUILD=<host>` permanently in the sysroot's `make.conf`, by design — real
crossdev sysroots keep that forever), so the exemption stayed "on" for
*any* package resolved against that sysroot, indefinitely — not just during
the toolchain build. Caught by the user's framing: "cross has special ways
to build the cross sysroot and toolchain, but to build a full stage1 they
aren't really needed since a cross stage1 is a normal stage1 with a
different compiler" — i.e. once the toolchain exists, ordinary stage1
packages (util-linux, e2fsprogs, acct-group/root, …) should get *full*
DEPEND resolution against the target, same as a native build; only the
toolchain-into-empty-sysroot bootstrap itself has the cycle problem
`--root-deps=rdeps` works around.

Fixed by making `--root-deps=rdeps` an explicit, caller-supplied input
instead of something auto-derived from `CHOST`/`CBUILD`:
- Removed `CrossContext::root_deps_rdeps()` entirely (dead code, single
  caller) — replaced `provider.set_root_deps_rdeps(cross.root_deps_rdeps(arch))`
  in `query::depgraph::depgraph()` with a plain `root_deps_rdeps: bool` field
  on `DepgraphOpts`, supplied by the caller.
- Added `--root-deps` to the `MergeFlags` mixin (mirroring real emerge's
  `--root-deps[=rdeps]`, modelled as a plain boolean since `rdeps` is the
  only value that ever exists) — per the CLI-mixin-scoping lesson above,
  *not* a plain global field.
- `em crossdev --setup`'s `setup()` forces `merge_flags.root_deps = true`
  unconditionally after computing the merged flags — matching real
  crossdev's `<CTARGET>-emerge` wrapper, which always implies the flag; not
  user-togglable there.
- `em toolchain --setup`, `em stages --stage1`, the bare `em <atoms>` path,
  and `equery depgraph` all default to `false` (full DEPEND resolution),
  overridable per-invocation via `--root-deps` if ever needed (e.g. to
  reproduce a similar bootstrap-cycle problem outside the crossdev flow).

Command → default table:
| command | `--root-deps` default | why |
|---|---|---|
| `em <atoms>` | off | ordinary install, not bootstrap |
| `em crossdev --setup` | **on**, forced | building the toolchain + glibc into a still-empty target |
| `em toolchain --setup` | off (moot) | native, `CHOST==CBUILD` always |
| `em stages --stage1` | off | toolchain already exists; ordinary packages |
| `equery depgraph` | off | display-only |

Verified: `em -p --root <dir> --cross riscv64-unknown-linux-gnu --autosolve-use
app-arch/libarchive sys-apps/util-linux` now resolves 34 packages (was 30) —
`acct-group/root` and `sys-fs/e2fsprogs` present. Passing `--root-deps`
explicitly reproduces the old (30-package) behavior, confirming the override
works both ways. Note: `-p`/`--pretend` never reaches `preflight::check`
at all (`emerge_atoms_inner` returns right after the depgraph exit-code
check, before `preflight::check` is even called) — so a clean `-p` run only
proves the *plan* is right, not that preflight agrees. See the real-run
follow-up below.

**14th finding: confirmed with an actual (non-pretend) run — the
`--root-deps` fix is correct and complete, but two more, pre-existing bugs
block the real build.** Ran `em --autosolve-use --keep-going -v --root
/var/tmp/cross-stage1-riscv64 --cross riscv64-unknown-linux-gnu stages
--stage1` for real (no `-p`) against the same riscv64 target. It failed at
`preflight::check` (before any package actually built) with 5 "needs:"
lines. `diff`'d against the very first pre-session capture of this exact
command (`cross-stage1-riscv64-stage1d.log`): the **only** change is
`sys-apps/util-linux-2.42.1 needs: acct-group/root` is gone (the fix), and a
new-looking `sys-fs/e2fsprogs-1.47.4 needs: sys-apps/util-linux` line
appeared — not a regression, just e2fsprogs's own pre-existing dependency
becoming visible for the first time now that e2fsprogs is finally in the
plan at all (before the fix it was silently dropped, so its deps were never
even checked). The other three lines were already present in the original
capture, untouched by any of today's work:
- `app-arch/libarchive-3.8.7 needs: sys-fs/e2fsprogs[abi_x86_32(-)?,…]`
- `sys-libs/libxcrypt-4.4.38-r1 needs: sys-libs/glibc[-crypt(-)]`
- `sys-devel/gcc-16.1.1_p20260606 needs: sys-libs/glibc[cet(-)?]`

Filed as a **very important, pending** blocker in `todo/PENDING.md` (top of
the stage-building section) rather than fixed here — two distinct bugs:
1. USE-dep conditional-default syntax (`flag(-)?`/`flag(+)?`/`-flag(-)`,
   EAPI 7+) not evaluated correctly — riscv64 lacks `abi_x86_*`/`crypt`/`cet`
   in IUSE entirely, so these should trivially pass regardless of arch.
2. `sys-apps/util-linux` install-order bug — both `e2fsprogs` and `python`
   DEPEND on it, but the solver places it *after* both (line 170 vs. 166/169
   in the plan) — a real topological-sort/edge-registration gap, not a
   preflight false positive.

Task #8 (`em stages --stage1` against a real `--cross` target) is now
blocked on these two, not on `--root-deps`/`acct-group/root` (that part is
done). Full command + log: `todo/PENDING.md`'s stage-building section,
top entry.
[[em-stages-and-binhosts]] [[crossdev-target]] [[em-root-characterization]]
