# Pending — stage-building arc (roadmap)

Open items from the toolchain → stage → binhost work, grouped. Each links to the
file with the detail. Status: 🔴 not started · 🟡 partial/decided · ✅ done (kept
here briefly for context). Updated 2026-06-27.

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
  `hakoniwa` 1.7.1; not wall-tested yet). Remaining: the binpkg/stage tar
  in-session (real `root:root` artifacts — next), fakeroot (system) backend,
  auto-detect chain, and per-package `__worker`. **Benchmark fakeroost vs hakoniwa
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
- 🔴 **packages.build DEPEND-into-ROOT residuals.** `acct-group/root`,
  `sys-fs/e2fsprogs`, util-linux ordering — re-test now that the DEPEND-trim
  sysroot fix landed; the staged toolchain pre-breaks the cycle. [[em-root-characterization]]
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
- 🔴 Consumer: remote `--getbinpkg` over `PORTAGE_BINHOST` (http(s) fetch + index) —
  *transport* is `portage-distfiles` (needs the `Packages.gz` fetch path), *format*
  (the reader) now exists. After `-k` local reuse.
- 🔴 `em stages` defaults to `--buildpkg` so each run feeds the next; per-arch.
- 🔴 Signing/verify (`BINPKG_GPG_*`) — last (lives in `portage-binpkg`).

## Other open (pre-existing, related)

- 🔴 clang linker config (Option B, `gentoo-linker.cfg`). [[select-toolchain]]
- See also [[nonemptytree-bdeps-gap]], [[em-emptytree]], [[build-clean-env]],
  [[crossdev-target]], [[cross-support-self-review]] for older open threads.
