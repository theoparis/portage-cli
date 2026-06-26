# Unprivileged builds: consolidate the chown workarounds behind a privilege backend

STATUS: **v1 landed (2026-06-27)** — umbrella fakeroost session + merge chown +
facet-2 name resolution, all validated on `sys-apps/util-linux`. **Next: tar/binpkg
(not started)** — see "START HERE next session" near the end. Goal: a correct
root-owned `@system` stage3 (setuid `mount`, `root:root`, file caps) without
running em as root. Supersedes the "decision point" in [[stage-build-shakeout]]
and the privilege half of [[build-clean-env]].

## Implemented — v1 (umbrella session)

Shipped the simplest correct slice (model B below), all in `main()` — no scheduler
or flock changes, since the whole build+merge stays in one process:

- `privilege.rs` — `Backend{RealRoot,Fakeroost,Sudo}` + `detect()` (RealRoot when
  euid==0 or already inside a session; else map the request, default Fakeroost) +
  `maybe_supervise()`. Selected by the global `--privilege <auto|fakeroost|sudo|
  none>` flag (clap, env `EM_PRIVILEGE`, so it shows in `--help`). `sudo` re-execs
  `sudo -E em …` for **real** root (root-owned tree + real setuid, catalyst-style),
  opt-in only / never auto-selected; `none` disables wrapping.
- `main()` calls `fakeroost::init()` first (before the tokio runtime), then for an
  unprivileged *building* invocation (`will_build`: emerge merge path +
  `ebuild`/`crossdev`/`toolchain`, not `--pretend`) re-execs em once under
  fakeroost (`EM_PRIVILEGE_ACTIVE` guards against re-supervising). The whole run —
  resolve, all builds, all merges — shares one ptrace+seccomp ownership table.
- `ebuild.rs` merge now `lchown`s each merged path to its image owner
  (`preserve_owner`) — a real gap even for **root** installs before this.
- The three EPERM workarounds are **kept** but now **inert under fakeroost**: each
  guards on `getuid`/`geteuid`/`EUID`, which fakeroost fakes to 0, so they take the
  privileged branch (real/faked chown, `0:0` default). They remain as graceful
  degradation when fakeroost is unavailable; remove once a real `@system` run
  confirms supervision is universal.
- Verified: `fakeroost` works on this kernel (`chown 0:0` unprivileged → `stat`
  reports `0:0`); em re-execs only on build paths, transparently.
- **Validated on the real wall (2026-06-27)**: `em --root /var/tmp/stage1-base
  sys-apps/util-linux` unprivileged — the package's own Makefile
  `chown root:root .../bin/mount` (the install-exec-hook that previously killed
  the build) now runs faked, util-linux merges (in VDB), and `mount` lands setuid
  (`-rwsr-xr-x`). On-disk owner is the build user (live unprivileged merge keeps
  it; the faked root owner is session-only — real `root:root` needs the in-session
  tar). This clears the wall that blocked `sys-apps/portage` and the self-extending
  `@system` base.

Deltas from the design: umbrella session instead of the per-package `__worker`
(deferred optimisation — keeps the resolver out of ptrace, enables independent
parallel sessions); RealRoot+Fakeroost+Sudo backends done, fakeroot/hakoniwa still
behind the seam. Facet 2 (target-passwd name resolution) is done (`907d914`).

---
Original design (the target end-state):

## The problem (one root cause, three patches)

Unprivileged builds cannot `chown` to root/foreign users. Today this is swallowed
in three places, each of which **discards** the intended ownership instead of
recording it — so the merged tree / binpkg / stage carries wrong ids, no setuid,
no file capabilities:

1. `build/stubs.rs` — bash `chown()`/`chgrp()` overrides return success on EPERM
   when non-root. Only catches chowns run *directly in ebuild bash*.
2. `build/commands/install.rs` `FownersCommand` — `fowners` shells to `chown`,
   swallows EPERM when non-root (`efdeb37`).
3. `build/commands/inst_owner.rs` — `PORTAGE_INST_UID/GID` default to the process
   uid in unprivileged mode so `install -o <self>` succeeds.

The deepest case escapes all three: util-linux's *own* Makefile
`chown root:root .../bin/mount` is a child-process chown, not interceptable by a
bash function. A real fake-root layer is required.

## What portage does (confirmed, portage-3.0.79)

- `FEATURES=fakeroot`, and only when `uid != 0` **and** `fakeroot_capable`
  (`/usr/bin/fakeroot` exists+executable) — `config.py:1492`, `doebuild.py:2098`.
- `process.spawn_fakeroot` (`process.py:172`) runs:
  `fakeroot -s ${T}/fakeroot.state -i ${T}/fakeroot.state -- bash -c <cmd>`.
- The **`-s`/`-i` state file is the crux**: portage spawns each phase as a
  *separate* `fakeroot` process, so the faked-ownership table must be **saved
  after install and re-loaded for qmerge/misc-functions** to carry ownership
  across the phase boundary (`MiscFunctionsProcess.py:47`, `EbuildPhase.py:124`).
- Applied to the install + merge phases only (not compile); orthogonal to the
  sandbox (`free`/`sesandbox`/`droppriv`) and `userpriv`.
- **Opt-in**: not in default FEATURES — a root merge does real chowns; fakeroot is
  the unprivileged / `ebuild` / binpkg path. em's directive is more aggressive:
  **auto-enable whenever unprivileged** (a deliberate divergence, noted below).

So portage = the external libfakeroot binary + a per-build state file. Backend
"fakeroot (system)" below mirrors it exactly; the others improve on it.

## fakeroost (confirmed from source: koca-build/fakeroost 0.1.1)

A **ptrace + seccomp** supervisor, pure Rust — *not* an LD_PRELOAD shim (so it
fakes ownership even for static binaries and under Docker's default seccomp).

- API: `fakeroost::init()` as the **first line of `main`** — detects the
  `__FAKEROOST_SUPERVISE` re-exec marker; a normal launch returns immediately, the
  supervisor launch runs the trace loop and exits with the child's status.
  `FakerootCommandExt::fakeroot()` on `std::process::Command` rewrites it to
  re-exec **our own binary** (`/proc/self/exe`) in supervisor mode, which then
  forks+traces the real target.
- **Whole-tree coverage**: the supervisor sets
  `PTRACE_O_TRACEFORK|TRACEVFORK|TRACECLONE`, so every descendant is auto-traced
  and shares **one** `OwnershipTable`. → covers em's **in-process** merge *and*
  every child (`make`/`install`/`chown`/eclass `chown 0:0`) in one session.
- Faked syscalls: `chown/lchown/fchown/fchownat` (record, skip real),
  `stat/lstat/fstat/newfstatat/statx` (overlay faked uid/gid/mode/rdev/nlink),
  `getuid/geteuid/...`→0, `setuid/.../capset`→success, `mknod/mknodat`
  (placeholder + record), `*xattr` (security.capability + ACLs), `chmod` (record +
  real). `unlink`/`rename` evict the table entry.
- Keyed by **(dev, inode)** — survives hardlinks/renames. Untracked ⇒ "owned by
  root" (real mode/rdev/nlink preserved).
- **No state save/load across processes** (no `-s/-i` equivalent): the table lives
  for the supervised run only.

## Why em is a *better* fit than portage's split model

em runs install **and** qmerge in **one** process — one carried build shell,
`build_and_merge` → `run_inner` over phases `[…, install, qmerge]` (`ebuild.rs:136`,
[[build-clean-env]]). So a single supervised worker holds install+qmerge in **one
in-memory table** — em needs **no `fakeroot.state` file** that portage requires
only because it spawns phases separately. The one requirement: the qmerge copy
(em's in-process `std::fs` at `ebuild.rs:1285`) must run *inside* the supervised
worker so the faked image-owner is visible and the chown into ROOT is recorded.

## `__worker`: the single entry point

`ebuild::build_and_merge` (`ebuild.rs:136`) is already the per-package
build+merge unit, and every argument is serializable except one:

| arg | worker flag |
|-----|-------------|
| `ebuild_path` | `--ebuild <path>` |
| `use_flags: &[Interned]` | `--use "a b c"` |
| `work_base` / `root` / `distdir` / `config_root` / `sysroot` / `eprefix` | matching flags |
| `quiet` | `--quiet` |
| `merge_gate: &Mutex` | **cross-process** flock on `work_base/.merge.lock`, held around qmerge |

Introduce a hidden `em __worker …` subcommand (mirroring the existing `em __helper`
precedent at `cli.rs:415`) whose body is one `build_and_merge` call. The dispatch
in `merge_sequential` (`main.rs:447`) and `merge_parallel` (`main.rs:583`) changes
from *call `build_and_merge` in-process* to *build the `em __worker` `Command`, let
the backend decorate it, spawn, await*. The `--jobs` scheduler (`Scheduler`,
`main.rs:483`) is untouched — it already awaits child build subprocesses.

`em`'s `main` gains `fakeroost::init()` as its first statement (a no-op unless this
exe was re-exec'd as the supervisor); the tokio `merge_gate` Mutex becomes an
flock so qmerge stays globally serial across worker *processes*.

## The `PrivilegeBackend` trait — the one seam

```
trait PrivilegeBackend {
    /// Spawnable command for one `em __worker` unit, wrapped in whatever
    /// provides root (fake or real) for the whole worker process tree.
    fn worker_command(&self, em_exe: &Path, args: &WorkerArgs) -> Command;
}
```

`detect()`: `euid==0` ⇒ `RealRoot`; else the configured backend, default **auto =
best available**. All backends converge on how `em __worker` is launched:

| backend | launch | family |
|---|---|---|
| **RealRoot** (root / `--jobs` in-proc) | `em __worker` (or keep in-process); real chowns | — |
| **fakeroost** *(default unpriv.)* | `Command::new(em).arg("__worker").fakeroot()` + `init()` in `main` | fake+acct |
| **fakeroot** (system) | `fakeroot -s/-i <state> -- em __worker` (portage's exact recipe) | fake+acct |
| **sudo** | `sudo em __worker` — real root, real setuid | real-in-box |
| **hakoniwa** | spawn `em __worker` in a userns sandbox, build-user→0 map (`~/Sources/hakoniwa`) | real-in-box |

"fake+accounting" (fakeroot/fakeroost) vs "real-in-a-box" (sudo/hakoniwa) are two
families behind the same `worker_command`. Auto-detect order when unprivileged:
fakeroost (pure-Rust, always linked) → fakeroot (binary on PATH) → hakoniwa
(userns available) → sudo (allowed) → degraded warn.

## What collapses once a backend records ownership

- `FownersCommand`: drop the EPERM-swallow → always real `chown` (faked + recorded);
  still resolve owner *name*→uid:gid against the **target** passwd/group (the second
  facet in [[stage-build-shakeout]]), then chown numerically.
- `stubs.rs` `chown`/`chgrp` overrides: **delete** — child chowns are faked for real.
- `inst_owner.rs`: back to portage's `0:0` default (the faker grants root).
- `ebuild.rs:1285` merge: **add** the missing chown — set each ROOT file's owner to
  its image-file owner (real when privileged, faked otherwise). This is a genuine
  gap even for **root** installs today: the copy never chowns, so non-root-owned
  files (`acct-user/*` dirs, etc.) land owned by whoever ran em. Ownership is **not**
  recorded in `CONTENTS` (it has no owner/mode field — like portage); it is captured
  at *archive* time instead — see Q1.

## Q1 RESOLVED — artifact ownership is captured at archive time, not stored

`CONTENTS` has **no** owner/mode field (confirmed: `portage_vdb::ContentsEntry` =
`{kind, path, md5, mtime, target}`, like portage), and fakeroost has **no**
cross-process state. So ownership cannot be reconstructed after a worker exits —
it must be read by the **archiver while it runs inside the fakeroost session** that
recorded the chowns. The resolution is therefore about *scoping the session to
cover the archiver*, and it splits by artifact:

- **Live unprivileged install** (`em --root <prefix>`): no artifact. fakeroost only
  stops the chown-EPERM death; on-disk files stay build-user owned (fine for a user
  prefix). Per-worker session suffices, nothing to preserve.
- **binpkg** (`em -b`): build the archive from the **image `${D}`** at end of
  `src_install`, *inside the same worker session* (the image already carries the
  faked chowns). Exactly portage's model (it packs the binpkg under fakeroot).
  → binpkg is the **canonical, durable carrier of ownership** (GPKG stores it in tar).
- **stage3** (`em stages`): do **not** tar a live unprivileged ROOT (its on-disk
  owners are build-user). Instead **assemble from binpkgs**: extract the
  already-correctly-owned binpkgs into a fresh ROOT and tar it, all under **one
  short umbrella fakeroost session** covering only extract+tar. Decouples the
  per-package builds (each a quick, parallel session) from the stage pack (one
  session over re-pack), and matches catalyst's "seed + packages → stage".

So no ownership store is added anywhere; the binpkg is the intermediate that holds
it, and every tar runs in-session. Detail in "Future: tar / binpkg" below.

## Open implementation questions

1. **Parallel workers each have their own table.** Independent is fine: each writes
   root:root into the shared ROOT; a later worker stat-ing those files gets the
   "untracked ⇒ root" default — correct for the common case. Only non-root-owned
   installed files (rare) could be misread cross-worker. Acceptable; revisit if it
   bites.
2. **Merge gate cross-process**: flock on `work_base/.merge.lock` vs a parent-held
   semaphore. flock is simplest and survives worker crashes.
3. **Worker arg round-trip**: `WorkerArgs` must fully reconstruct `build_and_merge`
   input; confirm the worker re-derives FEATURES/EPREFIX from `--config-root`
   rather than the parent's in-memory state.
4. **RealRoot stays in-process** (no spawn) for speed; spawn only when faking.
5. **fakeroost robustness on the 128-core `@system` run**: ptrace adds a per-syscall
   trap on the filtered set — measure overhead vs build cost; confirm it survives
   the heavy `make -j` trees.

## Future: tar / binpkg / stage artifacts (none exist yet)

`em -b`/`--buildpkg`, `--getbinpkg(only)` are **parsed flags with no
implementation** (`cli.rs:88-101`); there is **no archive-creation code** in the
tree. This is greenfield and is where Q1's "capture at archive time" lands.

### What the archiver must preserve (the fakeroost-specific traps)

fakeroost fakes ownership in its table, not on disk, so a naive `std::fs` walk sees
the *real* (build-user, placeholder) files. Correct output requires the archiver to
read through faked `stat`/`getxattr`, which only happens **in-session**. It must emit:

- **owner/gid** numeric, from faked `stat` (`--numeric-owner`; untracked ⇒ 0:0).
- **mode + setuid/setgid bits** (so `mount`/`ping` keep their bits).
- **device nodes**: fakeroost stores `mknod` as a *placeholder regular file* plus a
  recorded `(type, rdev)`; faked `stat` returns the char/block mode+rdev. The
  archiver must emit a real device-node tar entry **from the faked stat**, not copy
  the placeholder. (A plain copy would ship a 0-byte regular file.)
- **file capabilities / ACLs**: fakeroost fakes `security.capability` and ACL
  xattrs via `*setxattr`; the archiver must read them with faked `getxattr`
  (`tar --xattrs`, pax format).
- **hardlinks** (em's merge already tracks `(dev,ino)` at `ebuild.rs:1273`),
  symlinks, mtime.

→ Two implementation options:
- **(a) shell out to GNU `tar --numeric-owner --xattrs --format=pax`** inside the
  session — handles owners/devnodes/caps/hardlinks/ACLs natively, least code.
- **(b) a Rust archiver** (the `tar` crate doesn't do xattrs/devnodes out of the
  box) — more control, more code. Start with (a); revisit if a dependency-free
  static em matters.

### Format

Target **GPKG** (Gentoo's current default: an outer tar of `image.tar` +
`metadata.tar`, each optionally `zstd`/`gzip`, optional GPG per
`BINPKG_GPG_*`/`BINPKG_COMPRESS`). The legacy xpak `.tbz2` (tarball + appended
metadata blob) is read-only-compat territory; don't emit it. The binhost
`Packages` index and `--getbinpkg` consumer are tracked separately in
[[em-stages-and-binhosts]] / PENDING.md (binhost section).

### Where it slots in

- **binpkg**: a new step in the worker between `install` and `qmerge`, gated on
  `buildpkg`, packing `${D}` → `${PKGDIR}/<cat>/<pf>.gpkg` *within the worker's
  fakeroost session*. Metadata tar = the VDB env/`*.ebuild`/`USE`/deps em already
  computes for the VDB write.
- **stage3**: `em stages` final step = extract the selected binpkgs into a clean
  ROOT + emit the stage tar, under **one umbrella fakeroost session** (Q1). Ties
  into the privilege backend the same way — `em __stage-pack` could be a second
  worker-like entry point decorated by `worker_command`.

### Privileged path

Under `RealRoot`/`sudo`/`hakoniwa` the on-disk owners are already real (root, or
ns-mapped root), so the archiver needs no fakeroost session — option (a) `tar` just
works. The fakeroost path is the only one needing the in-session constraint; the
trait hides that (the archiver always runs via `worker_command`, which is a no-op
wrapper for RealRoot).

### START HERE next session (tar/binpkg — not started)

Context recap: `em -b`/`--buildpkg`/`--getbinpkg` are parsed-but-unimplemented
flags; there is **no archiver and no binpkg consumer** in the tree. The privilege
work (fakeroost umbrella, merge chown, facet 2) is done and validated, so the
in-session ownership the archiver needs is already there — `${D}` carries faked
`root:root` during `src_install`.

Steps, in order:

1. **Read the GPKG spec first** — `~/Sources/portage-3.0.79/lib/portage/gpkg.py`
   (and GLEP 78). Nail the exact container: outer **uncompressed** tar whose members
   are `<basename>/gpkg-1` (format marker), `<basename>/metadata.tar.<c>`,
   `<basename>/image.tar.<c>` (+ optional `.sig`), the per-member checksums, and how
   `<basename>` is formed. Don't guess the byte layout — em's GPKG must be readable
   by the host's real portage (the validation gate).
2. **image.tar** — option (a): shell to `tar --numeric-owner --xattrs
   --format=pax -C "${D}" .`, run **in the fakeroost session** so owner/devnode/cap
   reads are faked. Compress per `BINPKG_COMPRESS` (default `zstd`; `tar --zstd` or
   the `zstd` binary). Confirm setuid bits + `security.capability` survive.
3. **metadata.tar** — reuse the VDB metadata em already computes for the merge:
   `capture_environment` (→ `environment.bz2`), `CONTENTS`, and the xpak field set
   (`PF CATEGORY USE *DEPEND SLOT KEYWORDS IUSE repository …`). Find the VDB writer
   in `portage-cli/src/vdb.rs` + `ebuild.rs` and factor the field emission so binpkg
   and VDB share it.
4. **Assemble the container** + write to `${PKGDIR}/<cat>/<pf>.gpkg` (resolve
   `PKGDIR`, default `/var/cache/binpkgs`).
5. **Wire `--buildpkg`**: a new step in `run_inner`'s phase list between `install`
   and `qmerge` (`ebuild.rs:148`), gated on `cli.buildpkg`. It runs inside the same
   worker → already under the fakeroost session.
6. **Validate**: `em -b --root /var/tmp/stage1-base sys-apps/util-linux`
   unprivileged → inspect `image.tar` (`tar -tvf` shows `root/root` + setuid
   `mount`), then confirm the host's real portage reads it (`qtbsdtar`/gpkg tooling,
   or `emerge -K`/`--usepkgonly` against `PKGDIR`).

Open decisions to settle while reading gpkg.py: compression default + whether to
shell `tar --zstd` vs pipe `zstd`; whether to emit the `.sig`/Manifest checksums
now or defer signing (`BINPKG_GPG_*`); exact `<basename>` and `gpkg-1` marker
content. Deferred: the `--getbinpkg` **consumer** and the `Packages` index
(`em maint binhost`) — see [[em-stages-and-binhosts]]. stage3 re-pack (`em stages`
+ `em __stage-pack`) comes after the per-package binpkg works.
