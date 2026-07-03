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
