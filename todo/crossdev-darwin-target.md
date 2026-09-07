# `em crossdev --target arm64-apple-darwin*` — Darwin/XNU target support

Status: 🟡 core wiring landed and unit/live-verified through config-plan
generation; blocked short of a real package build by two **pre-existing,
Darwin-unrelated** infra gaps (below). Not started before this pass — `em
crossdev` only understood glibc/musl/newlib tuples.

## What landed

`-apple-darwinNN` tuples (e.g. `arm64-apple-darwin26.0.0`) are now a fourth
`Libc` model (`Libc::Darwin`, `crossdev/target.rs`), always LLVM (no GCC
bootstrap exists for Darwin — host `llvm-core/clang`+`llvm-core/lld` already
cross-target it, matching the LLVM model's own premise), with its own
package set instead of glibc/musl/newlib + llvm-runtimes:

```
host:   sys-devel/xcode-toolchain-wrappers, sys-devel/bootstrap-cmds,
        sys-devel/iig-tools, sys-boot/u-boot-xnu
target: sys-libs/libsystem, sys-apps/od-init, sys-kernel/xnu
```

(`sys-devel/apple-libtapi` is an empty placeholder dir in the overlay — no
ebuild yet — so it isn't in the set; add it once packaged.)

`gentoo_arch()`/`profile_path()` map to Gentoo Prefix's own `arm64-macos`/
`x64-macos` keyword family and this repo's `darwin-cross` overlay profile
(`profiles/targets/darwin/macos/<cpu>`), not `::gentoo`'s `default/linux/*`.

**The bigger structural change**: Darwin's packages are *real*, non-aliased
ebuilds in the `darwin-cross` overlay (own category names, own `KEYWORDS`),
unlike GCC/musl/newlib's `cross-<tuple>/pkg → sys-devel/gcc`-style alias onto
generic `::gentoo` ebuilds. So the one-source-repo assumption baked into
`alias_repo_conf_entry`/`cross_env_entries`/`sysroot_config_entries`/
`sysroot_repos_conf_entries` (all hardcoded to `::gentoo`) had to become
repo-name-parametric — see `CrossTarget::source_repo()` (`"gentoo"` normally,
`"darwin-cross"` for Darwin) and `crossdev::source_repo()`/`named_repo()` in
`mod.rs`. `ConfigEntry::Alias` gained a `source` field (was a hardcoded
`alias-source = gentoo` in `config_plan.rs`). The sysroot's own
`etc/portage/repos.conf` now also gets a `[darwin-cross]` entry (not just the
alias), so DEPEND chains inside the sysroot (e.g. `xnu`'s BDEPEND on
`iig-tools`) resolve the overlay directly.

`xnu-10063.141.1.ebuild`'s hardcoded `local darwin_sysroot="/usr/arm64-apple-darwin"`
was fixed to `"/usr/${CTARGET:-arm64-apple-darwin}"` — every other overlay
ebuild already did this, only `xnu` didn't, and a versioned tuple
(`arm64-apple-darwin26.0.0`, not the bare `arm64-apple-darwin` this overlay
was hand-configured for in `dev/portage/sysroot.make.conf`) exposed it.

**Live-verified** (unprivileged `--prefix`, this host's real
`/var/db/repos/gentoo` + `/var/db/repos/darwin-cross` → `overlay/`):

```
em crossdev --target arm64-apple-darwin26.0.0 --prefix DIR --show-target-cfg   # ✓ correct package set/profile/ARCH
em -p crossdev --target arm64-apple-darwin26.0.0 --prefix DIR --init-target   # ✓ correct config-change preview
em crossdev --target arm64-apple-darwin26.0.0 --prefix DIR --init-target     # ✓ writes make.conf/profile-symlink/
                                                                              #   repos.conf/package.env/package.accept_keywords correctly
em -p crossdev --target arm64-apple-darwin26.0.0 --prefix DIR --setup        # reaches step 1/8 (baselayout), then hits gap #2 below
```

56 existing crossdev unit tests + 10 config_plan tests: unaffected, still
green (`cargo test -p portage-cli --lib crossdev:: config_plan::`).

## Gap 1 — `--prefix` crossdev hits the already-tracked spurious blocker

Real (non-`-p`) `--init-target`/`--setup` under `--prefix` calls
`ensure_config_site_packages`, a real host merge of `sys-apps/config-site` +
`sys-devel/crossdev` — and that hits exactly
[[crossdev-prefix-spurious-os-headers-blocker]] (`sys-kernel/linux-headers`
vs `virtual/os-headers` blocker between two uninstalled packages). Confirmed
this is the *same* pre-existing bug, not a new one: identical shape, same
call path, unrelated to Darwin. Bare/privileged mode (the real deployment —
root inside the `dev/Dockerfile` container) is not documented as affected by
that report; not independently re-verified here (no root on this dev box).

## Gap 2 — profile system has no PMS cross-repo `repo:path` parent support

`--setup -p`'s plan preview gets past config-plan generation and into the
staged build, then dies immediately building the profile stack:

```
!!! failed to build profile stack: I/O error at
.../profiles/prefix/darwin/macos/14.0/arm64/clang/gentoo:prefix/darwin/macos/14.0/arm64/clang:
No such file or directory
```

The overlay's own darwin profile chain (`profiles/targets/darwin/macos/arm64`
→ `profiles/prefix/darwin/macos/14.0/arm64/clang`) correctly layers onto
Gentoo Prefix's *real* upstream Darwin profile via the standard PMS
cross-repo `parent` syntax:

```
gentoo:prefix/darwin/macos/14.0/arm64/clang
```

`Profile::parents()` (`portage-repo/src/repo/profile.rs`) does not implement
this — it only ever treats a `parent` line as a path relative to the current
profile dir (`self.path.join(l)`), so `gentoo:prefix/…` becomes a literal
(nonexistent) path component. This is a **general profile-system gap**, not
Darwin-specific — any overlay profile inheriting from its `masters` repo via
this syntax hits it, and it's real, documented Portage functionality
(`profile-formats` "portage-2"+).

Not fixed here: `ProfileStack::build`/`collect_stack`/`Profile::parents()`
take a bare filesystem path with zero repo-set context (no repos.conf, no
masters resolution) — ~40+ call sites across `portage-cli`/`portage-repo`/
`portage-resolve` (production and tests). Correctly resolving a `repo:path`
parent needs repos.conf-backed repo-name → path lookup threaded through that
whole chain, a properly-scoped feature on its own, not a bolt-on for this
pass. Needed before any Darwin package (or `em toolchain`/`em stages`
native bootstrap under this profile) can actually build end-to-end.

## Next steps

1. Fix gap 2 first — it blocks everything downstream, cross-repo profile
   parent resolution (repo name → path via repos.conf), threaded through
   `ProfileStack::build`.
2. Re-verify gap 1 in the real privileged deployment (root, `dev/run.sh`
   container) — confirm it's `--prefix`-only like the existing report says.
3. Once both clear: live-verify a real (non-`-p`) `--setup` — 8 steps,
   xcode-toolchain-wrappers → bootstrap-cmds → iig-tools → u-boot-xnu →
   libsystem → od-init → xnu — and iterate on whatever the first real build
   failure turns out to be (untested overlay ebuilds, per the user's own
   framing of this task).
4. Package `sys-devel/apple-libtapi` (currently an empty dir) if `--setup`
   ends up needing it as a host BDEPEND.
