# Pending — stage-building arc (roadmap)

Open items from the toolchain → stage → binhost work, grouped. Each links to the
file with the detail. Status: 🔴 not started · 🟡 partial/decided · ✅ done (kept
here briefly for context). Updated 2026-06-27.

## RESUME HERE (2026-07-05)

Mid riscv64 stage3 (`--emptytree @system`) shakeout, [[stage-build-shakeout]]
findings #22-27. Just fixed and committed (verified live): #24 PKGDIR
root-relative default + fail-fast preflight (`510e226`), #25 switched
`auto` privilege default to pseudoroot after a rare fakeroost ptrace
crash (`42d001e`), #26 three real dep-resolution bugs — USE-dep-blind
BDEPEND check (`762e645`), PKG_CONFIG not sysroot-scoped for `--cross`
(`1a7e7c4`), missing BDEPEND scheduling for `--cross` target packages
(`9c0354e`). `net-misc/dhcpcd`/`sys-apps/iproute2` reverified building
clean after all that. #27 (`dev-python/jinja2` dying in
`distutils-r1_python_install`) — ✅ **root-caused and fixed**: not a
missing directory at all — `capture_variables` (Compile→Install
worker-env dump) was leaking `PIPESTATUS` (and other bash dynamic vars)
across the process boundary; brush never re-resizes `PIPESTATUS` once
it's been explicitly `declare`d, so the eclass's `pipestatus || die`
check saw a stale, truncated array and misfired (`5902b73`). The
underlying brush bug itself is filed separately, not blocking:
[[brush-pipestatus-not-reset]]. #28 (new, found retrying the full run):
the merge-execution loop ignored each plan entry's own `merge_root`,
so a `MergeRoot::Host` BDEPEND like jinja2 silently built into the
`--cross` sysroot instead of `base_roots()` even after #26 correctly
scheduled it there — fixed via a new `entry_roots()` helper in `main.rs`
(`3ef21c8`). 57/59 packages now merge clean. #30: a "clean up this mess"
pass found the *same* bare-host hardcoding bug, independently duplicated,
in `Avail::initial_bdepend()` (used by `preflight::check()`) and
`bdepend_trim::TrimCtx`'s post-solve trim — both fixed to take
`host_roots` (`base_roots()`), mirroring #28's `load_host_installed` fix
(`732aefe`). #31 (asked "why did it fail though?" on the huge ~50-package
pre-flight list from retesting jinja2-into-`base_roots()`): found a
*second*, genuinely virtual cause mixed into that list — `check()`
checked every entry's own DEPEND against `depend_avail` regardless of
`merge_root`, but `depend_avail` only grows from Target merges, so a
`Host` entry's DEPEND on an *earlier Host*-merged package (e.g.
`dev-lang/perl` on `sys-libs/gdbm`, both routed to `base_roots()`)
spuriously failed. Fixed by branching the DEPEND check on `merge_root`
the same way the existing recording branch already does. #32 (found
immediately on the live re-run with #31's fix): a real, non-test-pollution
bug — `order`'s "already installed" filter checked `target_installed_cpvs`
(a `HashSet<Cpv>`, no `merge_root`) unconditionally, so a `Host`-routed
`dev-lang/perl` requirement (unsatisfied at `base_roots()`) matched the
`--cross` sysroot's own legitimately-installed perl by `(cpn, version)`
alone and got silently dropped from the plan — never built, BDEPEND
permanently unsatisfied. Almost misdiagnosed as stale-VDB pollution and
nearly deleted real completed target-system work before checking; caught
in time. Fixed by adding a parallel `host_installed_cpvs` set and
branching on `pkg.merge_root()`, same pattern as #28/#30/#31. This is
the fourth Host/Target root-conflation bug found this session. #33: live
re-run confirmed real progress (perl now appears in the plan at all) but
hit a *different, non-root* bug — perl's `[ebuild R]` reinstall entry is
appended by a separate fallback (`provider.reinstall_deps()` output
`install_order()` didn't naturally place) at the *end* of `order`,
landing after `dev-perl/YAML-Tiny` (its own dependent) in plan order, so
within-run visibility still doesn't see it in time. Not yet fixed —
paused here per explicit instruction to update the todo and step back:
review whether the Host/Target model (four point-fixes today, all at
the specific call site where each bug was found) is still sound, or has
become a pile of hacks needing real consolidation, before chasing #33.
**Remaining 1 failure** (of 2 — binutils resolved, see #29):
`sys-devel/binutils`'s `make exited 2` was real, not test-session noise —
confirmed straight from binutils' own upstream `Makefile.am`: it reuses
`ZSTD_CFLAGS` (the target sysroot's zstd include) in `AM_CFLAGS_FOR_BUILD`
(the native build-machine helper rule), tripping `#error unsupported ABI`
on aarch64-host/riscv64-target header mismatch. Upstream bug, not em's;
worked around with `sys-devel/binutils -zstd` in the sysroot's
`package.use` — verified rebuilding clean. `sys-apps/systemd-utils`
(blocked on jinja2/perl/python needing a full native stage1 at
`base_roots()`, this session's outer EROOT having only the minimal
cross-toolchain-support set — see [[em-root-characterization]] Tier 1
item 2) is the last known failure, but the exact remaining gap size is
still unconfirmed — #31 and #32 each fixed a real bug and moved the
plan closer to correct, but #33 (reinstall-fallback ordering) is a new,
separate, not-yet-fixed issue blocking the next live signal. Task #17
still in progress.

## Stage building (the active goal: a real stage3)

- 🟡 **Privilege / fakeroot for stage builds.** `sys-apps/util-linux`'s own
  Makefile `chown root:root .../bin/mount` fails unprivileged → blocks
  `sys-apps/portage` → no self-extending base. **v1 landed**: an unprivileged
  building invocation re-execs once under a fakeroost (ptrace+seccomp) umbrella
  session, so chown/setuid succeed and the merge records ownership; the three
  EPERM workarounds are now inert (fakeroost fakes getuid→0). **Validated**:
  `sys-apps/util-linux` merges unprivileged into `stage1-base` (the setuid-`mount`
  chown wall is cleared). ✅ Facet 2 — `fowners` resolves owner names to numeric
  uid:gid against the target passwd/group. ✅ `EM_PRIVILEGE=sudo` backend (real
  root, opt-in). ✅ `EM_PRIVILEGE=hakoniwa` umbrella sketch (userns mapped root,
  `hakoniwa` 1.7.1; not wall-tested yet). ✅ **Per-package `__worker` scoping
  (2026-07-01)**: fakeroost/sudo no longer umbrella the run — the un-wrapped
  parent runs `pretend..compile`, then a wrapped `em __worker` child runs
  install+qmerge(+binpkg) per package (Q6: the ptrace tax stays off the
  compile). Env crosses the process boundary via a variables-only `worker-env`
  dump (needed a brush `$'...'` parser fix, fork `6038e073`); qmerge is
  serialised across workers by an flock on `work_base/.merge.lock`; hakoniwa
  stays an umbrella; `em ebuild … install` keeps the umbrella (no worker seam).
  Validated: baselayout source build, `-b` producer and `-k` binpkg merge all
  through the worker. ✅ **Scoping confirmed live + fakeroost wrap fixed
  (2026-07-02, `f3201cb`)**: a uid/chown probe ebuild caught the worker wrap
  discarding `fakeroot()`'s returned command (silent degrade to `none`);
  full backend matrix now verified (uid/chown/gpkg ownership per phase).
  ✅ **pseudoroot backend (2026-07-02, `37e8d49` + `c6b0ae9`)**:
  `--privilege pseudoroot` = LD_PRELOAD fake root, worker-scoped like
  fakeroost, no ptrace tax; phase env passes `LD_PRELOAD`/`PSEUDOROOT_*`
  through exported. The two blocking pseudoroot bugs (supervise-marker env
  leak into the child + uid/gid default clobber) shipped fixed in the
  v0.2.0 release. **2026-07-03**: the util-linux gpkg sweep caught a third
  pseudoroot gap — the interposer missed the LFS `stat64` family, so
  bzip2 (ownership-preserving, binds `lstat64`) recorded the real build
  user on every compressed doc/man page (189/588 files); fixed in
  pseudoroot `f3997ea` (fakeroost verified immune — ptrace is
  symbol-agnostic). After that: 0/588 leaks, setuid mount/umount/su 0/0.
  Shipped as **pseudoroot v0.2.1**; workspace pins the tag (`5acb4ce`),
  path patch dropped, doc/man repro green from the plain git dep. Remaining: the
  binpkg/stage tar
  in-session (real `root:root` artifacts — next), fakeroot (system) backend.
  ✅ **`auto` now defaults to pseudoroot over fakeroost (2026-07-05,
  `42d001e`)** — a real riscv64 stage3 `--buildpkg` run hit a rare,
  non-reproducible-in-isolation fakeroost ptrace-supervisor crash
  (`fakeroost: syscall failed: ENOENT`) that silently killed ~1/3 of
  packages' install workers *after* qmerge had already succeeded; switched
  the priority order in `Backend::auto_backend()`. See
  [[stage-build-shakeout]] finding #25.
  **2026-07-03**: resumed the `stage-build-shakeout` @system run under pseudoroot
  — the util-linux wall is confirmed cleared. Found (a) a stale-VDB trap: any
  acct-group/acct-user package merged before a privilege backend existed lies
  about group/user creation (silent eclass no-op, not a failure) — needs
  re-merging, not a code fix; (b) ✅ **hang FIXED**: a `brush` scheduling
  deadlock — any read-side process substitution inside a command substitution
  (`old_groups=$(egetgroups …)` → `while read … done < <(…)` in
  `acct-user.eclass pkg_postinst`) strands the procsub body in a tokio worker's
  non-stealable LIFO slot while the parent blocks on a synchronous pipe read,
  so it never gets its first poll. Fixed with a `yield_now().await` after the
  procsub spawn (`setup_process_substitution`, `brush-core/src/interp.rs`);
  verified end-to-end (`@system` resumed clean, 50/50, 0 failures, no hangs).
  Patch sits **uncommitted** in the `~/Sources/brush` working tree
  (`for-portage-repo` branch) — needs Luca to review/commit/push + bump the
  `Cargo.toml` rev pin. [[stage-build-shakeout]] **Benchmark fakeroost vs hakoniwa
  vs sudo** — the 2026-06-27 stage3 smoke showed fakeroost (ptrace+seccomp, 2 ctx
  switches per `stat`/chown/…) much slower on the gcc bootstrap; if hakoniwa
  (userns, ~no per-syscall cost) lands near sudo it should become the default
  unprivileged backend. **2026-06-28 update**: fakeroost issue #7 fixed on PR #8
  (stat via a seccomp `USER_NOTIF` pool lifts the ceiling ~2.7×, and beats upstream
  `fakeroot` which goes backwards under load) — but a per-syscall tax remains, so
  the plan is to scope fake-root to `src_install`/archive only, not the compile.
  [[fakeroot-privilege-backends]] § Open Q6
  [[stage-build-shakeout]]
- 🟡 **`em stages`** — stage1 (`baselayout` + `packages.build`, built with the
  ROOT `<chost>-gcc` + SYSROOT=ROOT) → stage3 (`--emptytree @system`). No stage2
  (em builds a fresh toolchain, crossdev model). Needs `packages.build` ingestion
  and the CLI (the `package.use` `-*` colon gap below is now closed).
  [[em-stages-and-binhosts]]
- ✅ **`USE="-*"` clear-all** — now honoured across the USE/USE_EXPAND
  incremental merge (profile→globals→conf→env layers) and the shell-state read,
  so catalyst's `USE="-* build"` collapses the closure as expected.
- ✅ **`ACCEPT_LICENSE`/`ACCEPT_KEYWORDS` `-*`** — clear-all now honoured
  (`AcceptLicense::from_tokens` clears allow_all+allowed+denied;
  `AcceptToken::ClearAll` resets the accept decision, global and per-package).
- 🟡 **Remaining `-*` gaps are feature work, not patches:**
  - ✅ `package.use` USE_EXPAND colon form (`L10N: -* en`,
    `PYTHON_TARGETS: -* python2_7`) — `expand_use_expand_colon` (use_env.rs) parses
    `KEY:` group headers against the live USE_EXPAND keys, expands values to
    interned `UseOverride`s (no String detour), and treats a `-*` inside a group as
    "clear the group's live values, then trailing values rebuild it".
  - `ACCEPT_KEYWORDS` `-arch` removal still dropped (additive ArchAccept model).
  - `ACCEPT_PROPERTIES`/`ACCEPT_RESTRICT`/`PORTAGE_CHECKSUM_FILTER` — the vars
    themselves are unimplemented (zero refs); their GLEP-23 `*`/`-*` is moot
    until the vars exist.
  - `use.mask`/`use.force` correctly take only per-flag `-` (no `-*`, portage(5)).
  [[em-root-characterization]]
- 🟡 **Native toolchain activation via `em select`.** `em toolchain --setup`
  writes env.d profiles but no `usr/bin/<chost>-gcc` wrappers (post_step is a
  no-op). Blocker: `select/env_d.rs` is config-root-keyed, must be merge-root-aware
  for the activation path (trait-sig change across the four select modules). The
  stages need the ROOT `<chost>-gcc`. [[select-toolchain]]
- ✅ **`em stages --stage1 --cross` install-order/preflight bugs — FIXED
  2026-07-03.** Confirmed with real portage (`qdepends`) that the apparent
  `util-linux` ↔ `python` cycle was never real: util-linux's `python? (
  ${PYTHON_DEPS} )` doesn't apply with `python` off. Root cause: Level-C
  `--autosolve-use` ceding (`cede_required_use`,
  `portage-cli/src/query/depgraph/repo.rs`) scanned the *whole*
  `REQUIRED_USE` tree for flags to cede whenever *any* clause was violated,
  instead of just the violated clause(s) — util-linux's independently-satisfied
  `python? (...)` got ceded as a side effect of its unrelated, genuinely-violated
  `su? ( pam )` clause, fabricating a phantom `util-linux -> python` DEPEND
  edge that corrupted install order for the whole cluster (which is also
  what produced the "USE-dep conditional-default syntax" symptom below —
  once ordering is fixed, those self-resolve). Fixed by scanning only
  `ru.unsatisfied(&enabled)`'s clauses. Verified: phantom edge gone, order
  correct, real (non-pretend) `em stages --stage1 --cross riscv64...` now
  passes `preflight::check` clean and starts building (gcc underway).
  [[stage-build-shakeout]] finding #15.
- 🔴 **Profile/USE vs the releng stage profile.** em `@system` matches 175/180 of
  the real arm64 stage3; the 5 em-only (nghttp2/3, ngtcp2, libusb) are the default
  profile enabling curl `http2/http3/quic` + libusb vs the lean releng profile.
  Resolve against the same profile for apples-to-apples. [[stage3-vs-real-comparison]]
- 🔵 cosmetic: glibc post-install `failed to redirect to <root>/etc/hosts` (no
  /etc/hosts in a fresh ROOT). [[em-root-characterization]]

## Merge / build robustness (found in the @system shakeout)

- ✅ **CBUILD=CHOST** (`50081f2`) — python configure "cross" on native `--root`.
- ✅ **fowners non-fatal unprivileged** (`efdeb37`) — pam/eselect.
- ✅ **Merge unlink-before-overwrite** (2026-06-28). Re-merging over an existing
  read-only file (`bash` → `usr/bin/bashbug`, mode 0555) used to `Permission
  denied`: `walk_image` did a bare `std::fs::copy`, which opens the dest
  `O_WRONLY|O_TRUNC` → EACCES. Now unlinks the dest first (portage's behaviour),
  so the copy creates a fresh file (needs only directory write perm). Validated
  e2e: re-merge over `-r-xr-xr-x` files succeeds. [[stage-build-shakeout]]

## Distfile fetcher [[distfile-fetch-reliability]]

- ✅ **GENTOO_MIRRORS from make.globals** (`e0bae58`) — mirror fallback existed but
  the list was empty (never read make.globals).
- ✅ **Mirror filename-hash layout** (`distfile-fetch-reliability` C.4) — modern
  hashed-layout mirrors (`distfiles/<blake2b>/...`) honoured; flat path kept as a
  legacy fallback.
- ✅ **sourceforge HTML body rejected** (C.5) — a 2xx with `Content-Type: text/html`
  is treated as a fetch failure and the next URL is tried.
- ✅ **Corrupt partial refetched** (C.3) — resume only a size-plausible partial; on
  any verify failure discard + one fresh non-Range download.
- ✅ **Success-after-fallback registered** (C.2) — the per-distfile URL loop
  early-returns `Ok(Downloaded)` on the first success.
- ✅ **Computed `SRC_URI` (facet A) — DONE** (`2965fa2`, 2026-06-15). Global-scope
  loop/array-join construction (bash's `${my_urls[*]}`, the `bash53-001..015`
  patch loop) is evaluated correctly: the fetch phase reads `SRC_URI` from the
  already-sourced live shell via `is_phase_sourced`, not by re-sourcing. The
  original bug was re-sourcing no-op'ing eclasses (their include guards fire on
  the second pass) and dropping global-scope effects — leaving SRC_URI stale/empty.
  Verified: `em ebuild bash-5.3_p15.ebuild fetch` computes the full SRC_URI
  (tarball + 15 patches). Empty SRC_URI remains a legitimate state (84 meta/virtual
  ebuilds have `SRC_URI=""`), so no fail-fast on empty.
- ✅ **`em select mirrors` — DONE.** `list`/`show`/`set` with `--country`/`--region`
  filters; mirror list from Gentoo's XML API (`portage_distfiles::MirrorList`),
  writes `GENTOO_MIRRORS` to make.conf. `select/mirrors.rs`.

## Binhosts (fast stage3/stage4) [[em-stages-and-binhosts]]

- ✅ Producer: **`em -b` GPKG writer — DONE** (2026-06-28). New **`portage-binpkg`**
  crate (published `0.1.0` on crates.io) with the GLEP 78 writer (`write_gpkg`):
  container = plain tar `<PF>/gpkg-1` → `metadata.tar.zst` → `image.tar.zst` →
  `Manifest`, image via `tar --xattrs` pax (caps/devnodes), metadata = the VDB dir,
  `DATA … SHA512 … BLAKE2B` Manifest. `-b/--buildpkg` wired after qmerge (in the
  privilege session). **Validated: host portage reads, Manifest-verifies, and
  decompresses em's gpkg.** VDB enrichment 16→30 fields (PF, CHOST/C*FLAGS, FEATURES,
  INHERITED, DEFINED_PHASES, repository, NEEDED/NEEDED.ELF.2/REQUIRES/PROVIDES via
  the `object` ELF scan, the `.ebuild`). Format spec in
  [[fakeroot-privilege-backends]].
  - *VDB field follow-ups (down-scoped after investigating portage source):*
    `REPO_REVISIONS` is **not** a per-package VDB field — it is the repo
    git-revision-at-build-time, needs sync-history infra em lacks (the global
    `/var/lib/portage/repo_revisions`, which `emaint revisions` purges) → deferred.
    `IUSE_EFFECTIVE` is real but needs profile USE_EXPAND/arch plumbing the merge
    path doesn't thread → follow-up, not blocking.
- ✅ **GPKG metadata reader (`read_metadata`) + `em maint binhost` `Packages`
  index — DONE** (2026-06-28). `read_metadata` extracts a container's inner
  `metadata.tar.<c>` and returns the flat VDB field map (skips
  `environment.bz2`/the copied `<PF>.ebuild`). `em maint binhost` walks `PKGDIR`
  for `*.gpkg.tar`, reads each, and writes the `Packages` index in portage's
  `binarytree` format (sorted header + sorted per-CPV entries, `DESC`/`REPO`
  translations, `BUILD_ID`, container `MD5`+`SHA1`+`SIZE`+`MTIME`). **Validated
  against host portage: `binarytree.populate()` parses em's `Packages`, indexes
  the cpv, resolves SLOT/DESC/REPO/USE, zero invalids.** Commits `1b46a62`
  `413364f`.
- ✅ **`-k`/`--usepkg` local binpkg reuse — DONE & validated e2e.** The validity
  check (version matches by cpv lookup; USE restricted to the package's IUSE must
  match the desired USE — portage's `_match_use` bug-#453400 rule, so a stale-USE
  binpkg is rebuilt) + `BinpkgIndex` (reads the `Packages` index, scans PKGDIR as
  fallback) + `merge_binpkg` (extracts the image post-clean, runs only `qmerge`).
  `portage_binpkg::extract_image` added. **Validated end-to-end**:
  `em -b sys-apps/gentoo-functions` (build) → `em -k` into a fresh root merges
  byte-identical payload (matching md5sums, populated CONTENTS, no compilation).
  Commits `434ab22` + `5c74a01` (the latter fixed run_inner's clean wiping the
  pre-extracted image). [[em-stages-and-binhosts]]
- ✅ **`-g`/`--getbinpkg` remote consumer — DONE & validated e2e.** Transport
  (`portage_distfiles::fetch_index` — `Packages.gz` then `Packages`, gzip) +
  `fetch_binpkg` (streamed download via `.partial` rename). `RemoteBinpkgIndex`
  (same `use_compatible` rule, resolves to a download URL). `portage_binhosts`
  reads `PORTAGE_BINHOST`. Merge loop: `-g` implies `-k` (local overrides
  remote), `-G` is binpkg-only (no source fallback). **Validated**: served
  `Packages`+gpkg over http, `em -g` merged byte-identical payload; `-G` with no
  matching binpkg refuses to build. Commit `311d0f1`.
  - 🔴 **`binrepos.conf`** (modern format) — currently only `PORTAGE_BINHOST`
    (legacy, deprecated). `binrepos.conf` is INI with `[<name>]`, `sync-uri`,
    `priority`, `frozen`, `verify-signature`; recursed if a directory (no `.d/`).
  - 🔴 **`URI` header BASE_URI override** — portage resolves each entry's URL from
    the index's own `URI` header (server-controlled via
    `PORTAGE_BINHOST_HEADER_URI`), not the binhost's `sync-uri`. em uses
    `sync-uri`; both work when they match.
  - 🟡 **Remote-index freshness** — em fetches the index fresh each run; portage
    caches at `/var/cache/edb/binhost/<host>/<path>/Packages` with `TTL` +
    `If-Modified-Since` (304 → reuse). Flagged.
  - 🟡 **gpkg GPG signature verify** — `binpkg-request-signature` FEATURE / repo
    `verify-signature=true` (default-on in shipped config) drops remote XPAK and
    GPG-verifies gpkg at unpack. em accepts unsigned. Last (with signing).
  - 🟡 **`-K`/`--usepkgonly` enforcement** — local-only binpkg mode, no source.
    The flag exists but isn't enforced (the merge loop falls through to build).
    Symmetric to the `-G` enforcement now wired.
  - 🔵 **`binpkg-multi-instance` BUILD_ID** — multiple instances per cpv keyed by
    `(cpv, BUILD_ID, …)`. em keys by cpv (one instance). Rare in practice.
  - 🔴 **Per-package build-env provenance / CFLAGS gating (RVV).** The `Packages`
    format is `KEY: VALUE` so per-package `CFLAGS`/`CXXFLAGS`/`LDFLAGS`/`CBUILD`/
    `FEATURES` are syntactically valid, and the data already lives in each GPKG's
    `metadata.tar` (em writes them during merge). But portage's reader silently
    drops unknown per-package keys (`SlotDict` filter on `_pkgindex_allowed_pkg_keys`)
    — so lifting them into em's index is an **em-only extension**, invisible to
    portage. portage deliberately matches on CHOST+USE+ABI (sonames) only and
    trusts the operator avoids `-march=native`; that model breaks for
    **riscv64 RVV variants** — a `-march=...v` binpkg won't run on a core without
    the V extension, so CHOST+USE match is unsafe. The fix is option 1: write the
    build-env fields into em's `Packages` and gate `find_reusable` on `-march`
    (opt-in). Deferred (later) — non-riscv64 CHOST+USE+ABI matching is portage-
    faithful for now.
- 🔴 **`em maint binpkg` tooling** — the binhost substrate (Packages index + reader
  + local/remote reuse) now invites `maint` family tools operating uniformly on
  local PKGDIR and remote-cached binpkgs: `verify` (the `BinpkgVerifier` MD5/SHA1/
  size integrity check), `list`/query, prune-old-`BUILD_ID`s (eclean-pkg
  workalike). None built yet.
- 🔴 `em stages` defaults to `--buildpkg` so each run feeds the next; per-arch.
- 🔴 Signing/verify (`BINPKG_GPG_*`) — last (lives in `portage-binpkg`).

## Other open (pre-existing, related)

- 🔴 **Parser audit pass** — review the recent burst of parser work (incremental
  `-*`, package.use/license/accept_keywords, @set expansion, USE-dep eval, IUSE
  defaults, make.conf sourcing, md5-cache) for PMS/portage faithfulness.
  [[parser-audit]]
- 🔴 clang linker config (Option B, `gentoo-linker.cfg`). [[select-toolchain]]
- See also [[nonemptytree-bdeps-gap]], [[em-emptytree]], [[build-clean-env]],
  [[crossdev-target]], [[cross-support-self-review]] for older open threads.
