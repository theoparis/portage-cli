# Pending — stage-building arc (roadmap)

Open items from the toolchain → stage → binhost work, grouped. Each links to the
file with the detail. Status: 🔴 not started · 🟡 partial/decided · ✅ done (kept
here briefly for context). Updated 2026-06-26.

## Stage building (the active goal: a real stage3)

- 🔴 **Privilege / fakeroot for stage builds.** `sys-apps/util-linux`'s own
  Makefile `chown root:root .../bin/mount` fails unprivileged → blocks
  `sys-apps/portage` → no self-extending base. A full `@system` stage with setuid
  binaries needs **root or fakeroot** (catalyst runs as root). Decide: run `em` as
  root for stages (simplest, root-owned tree) vs integrate fakeroot (keep the
  unprivileged model). **The decision point.** [[stage-build-shakeout]]
- 🟡 **`em stages`** — stage1 (`baselayout` + `packages.build`, built with the
  ROOT `<chost>-gcc` + SYSROOT=ROOT) → stage3 (`--emptytree @system`). No stage2
  (em builds a fresh toolchain, crossdev model). Needs `packages.build` ingestion,
  the `-*` USE gap below, and the CLI. [[em-stages-and-binhosts]]
- ✅ **`USE="-*"` clear-all** — now honoured across the USE/USE_EXPAND
  incremental merge (profile→globals→conf→env layers) and the shell-state read,
  so catalyst's `USE="-* build"` collapses the closure as expected.
- ✅ **`ACCEPT_LICENSE`/`ACCEPT_KEYWORDS` `-*`** — clear-all now honoured
  (`AcceptLicense::from_tokens` clears allow_all+allowed+denied;
  `AcceptToken::ClearAll` resets the accept decision, global and per-package).
- 🟡 **Remaining `-*` gaps are feature work, not patches:**
  - `package.use` `-*` — the documented form is the USE_EXPAND colon syntax
    (`L10N: -* en`), which isn't parsed at all; plain `-*` is rare. Needs colon
    parsing + USE_EXPAND-aware clearing + a `UseOverride` type change.
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
- 🔴 **Merge unlink-before-overwrite.** Re-merging over an existing read-only file
  (`bash` → `usr/bin/bashbug`, mode 0555) → `Permission denied`; em writes without
  `unlink`/chmod first. Only bites on re-merge. Clean fix. [[stage-build-shakeout]]

## Distfile fetcher [[distfile-fetch-reliability]]

- ✅ **GENTOO_MIRRORS from make.globals** (`e0bae58`) — mirror fallback existed but
  the list was empty (never read make.globals).
- 🔴 **Mirror URL uses flat layout** → 404 on modern hashed-layout mirrors
  (`distfiles/<blake2b>/...`). Honour the mirror `layout.conf`.
- 🔴 **sourceforge SRC_URI yields an HTML/redirect body** (accepted as the file →
  verify fails). Reject `text/html`/too-small 2xx; try next URL.
- 🔴 **Success-after-fallback still marked failed** (tar: 404 then ok, still
  reported failed) — per-file result accounting.
- 🔴 **Corrupt partial never refetched** (psmisc: bad cached file Range-resumed
  into garbage). Discard + fresh download on verify failure.
- 🔴 **`em select mirrors`** — `eselect mirror`/mirrorselect workalike
  (list/set/rank, writes GENTOO_MIRRORS). [[select-toolchain]]

## Binhosts (fast stage3/stage4) [[em-stages-and-binhosts]]

- 🔴 Producer: confirm `em -b` PKGDIR + `em maint binhost` `Packages` index (GPKG).
- 🔴 Consumer: remote `--getbinpkg` over `PORTAGE_BINHOST` (http(s) fetch + index).
- 🔴 Binpkg reuse/rebuild via the solver's USE/ABI/slot machinery.
- 🔴 `em stages` defaults to `--buildpkg` so each run feeds the next; per-arch.
- 🔴 Signing/verify (`BINPKG_GPG_*`) — last.

## Other open (pre-existing, related)

- 🔴 clang linker config (Option B, `gentoo-linker.cfg`). [[select-toolchain]]
- See also [[nonemptytree-bdeps-gap]], [[em-emptytree]], [[build-clean-env]],
  [[crossdev-target]], [[cross-support-self-review]] for older open threads.
