# Build roadmap: from `em sys-apps/less` to `em firefox`

Status tracking for the build/merge pipeline. Checked items are implemented
*and verified on this host*; the proof column says how. Resolver-side work
(preview parity, multi-repo, Level-C) has its own docs and is not re-listed.

Conventions: every milestone ends with a named **gate** — a concrete command
that must succeed before moving on. Fix forward only what the gate needs;
park everything else here.

## M0 — Foundations (done)

- [x] `em -p` plan parity with `emerge -p` (8-target basket, exit codes,
      preview semantics) — `benchmarks/bench-em-vs-emerge.sh`
- [x] Multi-repo: repos.conf overlays, sourced metadata + user cache,
      crossdev symlinks, `em regen` — cross-riscv64 gcc resolves `[R] 16.1.0`
- [x] `em ebuild <path> <phases>`: fetch (RO-fallback distdirs), unpack,
      src_* chain, install, merge (collision check, slot-occupant unmerge,
      CONTENTS/counter/environment.bz2, pkg_preinst/postinst)
- [x] Hermetic per-ebuild sourcing (baseline shell snapshot)
- [x] `em <atoms>`: resolve → build loop (per-package effective USE from the
      plan) → qmerge, refusing on pending USE changes
- [x] `--prefix DIR`: ROOT+VDB+distfiles+build trees under DIR, unprivileged
      — gate was `em --prefix /tmp/p sys-apps/less` → binary runs, PCRE2 USE
      applied

## M1 — Single-package robustness

Goal: a leaf-package build is trustworthy and debuggable.

- [x] `src_test`: skipped by default in the merge chain, run under
      `FEATURES=test` (explicit `em ebuild … test` always runs it)
- [x] FEATURES parsing from the configured shell (profile + make.conf):
      `test` and `keepwork` acted on, the rest accepted silently (`nostrip`
      is a no-op until stripping exists; collision check is always-on)
- [x] Per-package build log (`<workdir>/build.log`, tee'd via process
      substitution; path attached to failures) and `-q` captured-silent mode.
      Required teaching Rust-builtin children (econf/emake) to honour the
      shell context's redirected fds (`context_stdio`)
- [x] Profile build environment in the phase shell: `make.defaults` vars
      (CHOST, CFLAGS/LDFLAGS defaults, `MULTILIB_ABIS`, `ABI`, `LIBDIR_*`,
      `USE_EXPAND` values) via `ProfileStack::configure_shell`; the plan's
      per-package USE overrides on top — file-5.47 now builds in
      `file-5.47-.arm64` with libs in `/usr/lib64`
- [x] `pkg_pretend` + `pkg_setup` in the merge chain with correct
      `EBUILD_PHASE`/`MERGE_TYPE` (both were already wired; chain extended)
- [x] die-in-subshell: `die` now raises an Arc-shared `DieFlag` visible to
      the phase driver after the phase returns, so a die inside `$(...)` or a
      helper pipeline aborts the build (portage's marker+signal, in-process).
      This flipped two silent corruptions into real failures and led to their
      fixes: `has_version`/`best_version` were metadata stubs returning
      false — they are real VDB-querying builtins now (`-b/-d/-r` against
      BROOT/ESYSROOT/ROOT), un-stubbed for phases; and `econf` was missing
      the PMS `--libdir=${EPREFIX}/usr/$(get_libdir)` argument, so xz-utils
      installed its libraries to `usr/lib` and failed its own sanity check.
      Also: `TMPDIR` joined the phase export list (eltpatch wrote to
      `/libtool-elt.patch`).
- [x] Leaf-basket hardening run (2026-06-11, all unprivileged into fresh
      prefixes, die enforcement active):

      | package | result |
      |---|---|
      | sys-apps/file (autotools+multilib) | PASS |
      | app-arch/gzip (network fetch) | PASS |
      | sys-devel/bc | PASS |
      | app-arch/zstd (meson) | PASS |
      | app-arch/xz-utils (autotools, libdir-sensitive) | PASS |
      | sys-apps/sed | PASS |
      | sys-apps/less (eautoreconf) | PASS |

**Gate:** `em --prefix /tmp/p app-arch/zstd` (meson, as it turns out) and
`sys-apps/file` (multilib) both merge with correct ABI libdirs and a saved
build.log — **passed 2026-06-11**, and **M1 is complete** (7/7 basket with
die enforcement). The parked `command not found: -E` wart turned out to be
two real bugs, both fixed: `usex`'s value positionals rejected
hyphen-leading arguments (meson.eclass's `meson_use`/`meson_feature` emit
`-D...=true/false`), and brush's `export` silently dropped runtime
assignments from expanded words (`export ${var}=value` —
toolchain-funcs.eclass `_tc-getPROG`), so every `tc-getCC` lookup returned
empty and `$($(tc-getCC) -E -P -)` ran `-E` as a command. Fixed in the
brush fork (with bash-oracle compat cases) — builds now get the proper
CHOST-prefixed compiler exported.

## M2 — Multi-package orchestration

Goal: dependency chains build in order and failures are resumable.

- [ ] Pre-flight dependency check: each plan entry's DEPEND/BDEPEND must be
      satisfied by host VDB ∪ already-merged-this-run (prefix VDB); clear
      error naming the missing tool otherwise
- [ ] Within-run visibility of earlier merges: later builds see
      `<prefix>`-installed deps (PATH, PKG_CONFIG_PATH, CMAKE_PREFIX_PATH /
      or document host-BROOT semantics — decide ROOT-vs-BROOT story for
      prefix builds)
- [ ] `--keep-going` + resume: record per-entry state; rerun continues from
      the first unmerged entry
- [ ] `--ask` prompt before the loop
- [ ] Failure report: package, phase, log path, one-line cause
- [ ] `--jobs N`: parallel builds respecting the dependency order (use the
      solver's edges, not just list order)

**Gate:** a 2–3 package uninstalled chain (e.g. `app-text/tree` style leaf +
a lib dep) merges into a fresh prefix in one `em --prefix` invocation;
killing it mid-way and rerunning completes without rebuilding done entries.

## M3 — Sandbox & safety

Goal: phases stop running raw on the host.

- [ ] Decide mechanism: mount/user namespaces (bubblewrap-style, no root) vs
      portage's LD_PRELOAD sandbox vs both-tiered; write the decision here
- [ ] Write-confinement: build can write only WORKDIR/T/D (+ DISTDIR for
      fetch); violations logged, fatal under `FEATURES=strict`-equivalent
- [ ] Network off during src_* phases (fetch is the only network phase)
- [ ] `userpriv` semantics for root invocations (drop to a build user)

**Gate:** a deliberately misbehaving ebuild (writes to `$HOME`, phones home
in src_compile) is caught by both confinements.

## M4 — Heavy-stack eclass coverage (the firefox prerequisites)

Goal: the eclass machinery firefox's stack needs works under our shell.
Iterate target-by-target, hardest last:

- [x] meson package end-to-end — `app-arch/zstd` (meson build system) merged
      into a prefix with working binary and libraries
- [ ] cmake package end-to-end (`app-arch/zstd` if not done in M1)
- [ ] python-any-r1 BDEPEND package (host python detection, no target installs)
- [ ] `check-reqs` (needs /proc memory introspection), `multiprocessing`,
      `toolchain-funcs` audit under our shell
- [ ] llvm-r1 slot detection against host LLVM
- [ ] cargo eclass: vendored-crates SRC_URI unpack (`cargo_src_unpack`),
      offline `cargo build`; small rust package end-to-end first
- [ ] ebuild-helpers coverage audit: we reuse the host portage's
      PORTAGE_BIN_PATH helpers — list what firefox's install phase calls
      (dostrip/ecompress/...) and verify each
- [ ] firefox dry-run ladder: `setup → unpack → configure` first, catalog
      failures here, fix, extend to compile

**Gate:** `em --prefix /tmp/p www-client/firefox` completes
setup→configure. (Full compile is hours of CPU — gate on configure, run
compile once overnight when configure is clean.)

## M5 — emerge UX completeness (post-firefox, ordered by value)

- [ ] `-b`/`--buildpkg`: binary package creation on merge (decide xpak
      vs gpkg first — gpkg is the future, xpak interops with existing hosts)
- [ ] `quickpkg` from installed files (currently a stub)
- [ ] `-K`/`--usepkg`: install from binpkg, skipping build phases
- [ ] `em -C` unmerge (the slot-occupant unmerge logic already exists —
      expose it standalone)
- [ ] `@world`/`@system` set resolution + `--update --deep` semantics
- [ ] `--fetchonly`

## M6 — Prefix polish

- [ ] Prefix bootstrap helper: create baselayout dirs, optionally seed
      `etc/portage` (then `--prefix` could also offset config — today config
      is host-only by design)
- [ ] Environment entry script (`<prefix>/start` exporting PATH/LD_LIBRARY_PATH)
- [ ] Document the ROOT/BROOT/EPREFIX story vs Gentoo Prefix proper

## Standing items (not milestone-gated)

- [ ] Push master to origin (43+ commits ahead)
- [ ] Run `benchmarks/bench-em-vs-emerge.sh` after each milestone; parity
      regressions block
- [ ] brush upstream: `todo/checkpoint.md` (checkpoint/restore API)
- [ ] pubgrub upstream: portage-cli#1 ↔ pubgrub-rs#120 (multi-literal
      incompatibilities)
- [ ] Blockers/`::repo` Tier-1 enforcement (advisory today; `::repo` newly
      testable on this host)
