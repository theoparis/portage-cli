# `em select` — toolchain activation (gcc / binutils / linker / clang)

Consolidates the former `select-{compiler,binutils,linker,clang}.md`. These are
the `eselect`/`*-config` workalikes that *activate* a built toolchain (write
`env.d` state + the `usr/bin/<T>-*` wrappers). The build half is done; this is
the activation half — the seam where `em select` meets the toolchain/stages work
([[em-stages-and-binhosts]], [[em-root-characterization]], [[crossdev-target]]).

## Implemented (2026-06-24), all in `portage-cli/src/select/`

- **compiler** (`gcc-config`/`eselect gcc`): `list`/`show`/`set`, per-arch
  grouping, `--config-root`/`--local`/`--prefix`-aware. `install_wrappers` reads
  env.d `GCC_PATH` → `usr/bin/<T>-<tool>` + `<T>-cc`, re-rooted under EPREFIX.
- **binutils** (`binutils-config`): same shape; two-level layout
  `usr/libexec/gcc/<T>/<tool>` → `binutils-bin/<VER>`, then `usr/bin/<T>-<tool>`.
  Bin dir located on disk (cross nests `usr/<CBUILD>/<T>/binutils-bin`, native
  `usr/<T>/binutils-bin`).
- **linker**: ld/lld/mold selection via the same `env.d/linker/` mechanism.
- **clang** (Option A): LLVM *slot* selection (`/usr/lib/llvm/<SLOT>`), distinct
  from the env.d mechanism. `list`/`show`/`set`.

Shared engine: `select/env_d.rs` (the `EnvDProfile` trait + `activate_latest`,
`set_profile`, `install_wrappers`).

## Auto-activation from the build — the real consolidation

The ebuild `pkg_postinst` runs the **host** `binutils-config`/`gcc-config`, which
activates into `/`, not the build root. So the merge driver must activate via
`em select` after the toolchain steps. Status by flavour:

- **cross** — DONE. `crossdev --setup`'s `post_step_cross` calls
  `select::activate_binutils` then `select::activate_compiler` (gcc references the
  binutils tools, so binutils first), plus `link_abi_osdirs` after libc.
- **native** (`em toolchain --setup`) — **NOT done; the open item.** Its
  `post_step` is a no-op. Empirically (capstone `/var/tmp/stage1-capstone`): the
  ebuilds write `etc/env.d/{gcc,binutils}/<chost>-<ver>` into the ROOT, but the
  `usr/bin/<chost>-{gcc,as,ld,…}` and `gcc`/`cc` wrappers are **missing** — so the
  ROOT toolchain is reachable only by full `gcc-bin` path. This blocks the stages
  (they must invoke the ROOT's `<chost>-gcc`, see [[em-stages-and-binhosts]] #5).

### Blocker: `env_d` is config-root-keyed, must be merge-root-aware

`select/env_d.rs` derives every path from `config_portage_dir` (the **config
root**). For a native `em toolchain --setup --root <R> --config-root /`:
- `list_all_profiles` reads `<config>/etc/env.d/...` = the **host** profiles, not
  `<R>`'s freshly-built one → `activate_latest` would pick a host profile.
- `set_profile` + `install_wrappers` write under `env_d::eprefix` = the config
  root → wrappers land on the **host**, not `<R>`. Wrong (and harmful).

The toolchain's env.d + wrappers belong in the **install target (merge_root)**,
because that's where the binaries are. Fix: thread an explicit
`eprefix = roots.merge_root()` through the *activation* path
(`activate_latest → set_profile → install_wrappers`, plus the profile-read in
`list_all_profiles`/`env_d_dir`), leaving the user-facing CLI (`list`/`show`/`set`)
on the config root. Note `install_wrappers` is a trait method on `EnvDProfile`, so
the signature change touches all four modules + their tests — a contained but
real refactor, hence not done in the unslop pass.

For the common cases this is a no-op: plain (`merge_root == / == config`),
`--local` (`merge_root == ~/.gentoo == overlay root`), and `--cross`
(`merge_root == sysroot == config`). Only `--root --config-root /` diverges —
exactly the toolchain/stage case we need.

Then wire native: factor `crossdev::activate_toolchain` to take a `tuple: &str`
(it only uses `target.tuple`), give `em toolchain --setup` a `post_step` that
activates with `select::get_chost(globals)` as the tuple (no `link_abi_osdirs` —
that's cross-only), and cross keeps its current hook. One shared activation seam.

### Validation
`<R>/usr/bin/<chost>-gcc hello.c -o hello --sysroot=<R> && file hello` →
working ELF; and the stages then build via `<chost>-gcc`. (Today the same works
only via the full `usr/<chost>/gcc-bin/<ver>/<chost>-gcc` path.)

### 2026-07-16 re-check: confirmed live, and the gap is one layer deeper than written above

Verified against real, already-built native toolchain roots on this host
(`/var/tmp/stage1-native`, `/var/tmp/stage1-base`, both aarch64-on-aarch64):
`usr/bin/` has only gcc's own versioned install (`aarch64-unknown-linux-gnu-
gcc-16`), no bare `<chost>-gcc` wrapper, no `gcc`/`cc`; `etc/env.d/gcc/` has
only the ebuild-written profile file, no `config-<chost>` — confirming
`activate_latest`/`set_profile` never ran for these, exactly as `post_step`
being `|_| Ok(())` in both `toolchain()` and `stage1()` (`crossdev/mod.rs`)
predicts.

**Second, deeper gap found tracing what would actually consume that wrapper**:
`portage-repo/src/build/shell.rs`'s cross-toolchain-selection block
(~line 1170-1220, sets `CC`/`CXX`/`PATH` to `<chost>-*`) is explicitly gated
on `chost != cbuild`, skipped for "native (CBUILD unset, or CHOST == CBUILD)"
by design — and a native `--root` build always ends up `CHOST == CBUILD`
(an earlier block in the same file defaults `CBUILD` to `CHOST` when unset).
`PATH` itself (~line 1071-1085) is built from the **host's own**
`/usr/bin:/bin` (sanitised of `$HOME`/`/usr/local`), with no ROOT-scoped
prepend in the native case. So even after `em select`'s wrapper gap is fixed,
nothing in the native build path would put that wrapper ahead of the host's
real `gcc` on `PATH` — a native stage build's `CC` silently resolves to the
**host's** compiler today, not the freshly-built ROOT one. This is exactly
the "host lib leaks in" failure mode `em-stages-and-binhosts.md`'s design
question #5 warns about; it just wasn't previously pinned to this specific
code path.

Closing this needs two changes, not one: (a) this doc's already-planned
`env_d.rs` merge-root-awareness + a real native `post_step` (unblocks `em
select`/the wrapper itself), and (b) a native-offset branch in `shell.rs`'s
toolchain selection, parallel to the existing cross one, that prepends the
ROOT's own toolchain dir to `PATH` even when `CHOST == CBUILD`. (a) alone
creates the wrapper but doesn't make the build shell use it.

### 2026-07-16, continued: checked against real portage/crossdev's own model — (b) is a *widened condition*, not a new branch

Read `toolchain-funcs.eclass` (`/var/db/repos/gentoo/eclass/toolchain-funcs.eclass`)
and real crossdev's own `<tuple>-emerge` wrapper (`/usr/bin/riscv64-unknown-
linux-gnu-emerge`) to check em's model against the real one, since (b) above
was framed as "add a native branch" — that's not quite what real portage does.

**Real portage's algorithm** (`_tc-getPROG`, `tc-getPROG`/`tc-getBUILD_PROG`):
for every one of `tc-getCC`/`tc-getCXX`/`tc-getAR`/`tc-getAS`/`tc-getLD`/
`tc-getNM`/`tc-getRANLIB`/`tc-getSTRIP`/`tc-getOBJCOPY`/`tc-getOBJDUMP`/
`tc-getCPP`/`tc-getF77`/`tc-getFC`/`tc-getPKG_CONFIG`/`tc-getRC`/`tc-getDLLWRAP`/
`tc-getGCJ`/`tc-getGO`/`tc-getHIPCXX` (and their `tc-getBUILD_*` twins, keyed
off `CBUILD` instead of `CHOST`):
1. If the plain var (`CC`, `AR`, …) is already exported, use it verbatim —
   **no CHOST check at all**. (`tc-getBUILD_*` also checks `BUILD_CC`/
   `CC_FOR_BUILD`/`HOSTCC` first, plus the bare var too when
   `tc-is-cross-compiler` is false.)
2. Otherwise, search `$PATH` for `${CHOST}-<tool>` (or `${CBUILD}-<tool>` for
   the `BUILD_*` family) and use the resolved short name if found.
3. Otherwise, fall back to the bare tool name (`gcc`, `ar`, …).

Critically, **step 2 always runs, native or cross** — `tc-is-cross-compiler`
(`${CBUILD:-CHOST} != CHOST`) only changes whether the `BUILD_*` family also
accepts the bare var as a last resort; it never gates the CHOST-prefix search
itself. This is *why* a plain, non-offset Gentoo system's `tc-getCC` still
finds `${CHOST}-gcc` (e.g. `x86_64-pc-linux-gnu-gcc`) — `gcc-config` always
creates that CHOST-prefixed symlink, cross or not, and real portage's own
build always goes through it, not a bare `gcc`.

**Real crossdev's `<tuple>-emerge` wrapper doesn't set `CC`/`CXX`/... at all**
— only `CHOST`, `SYSROOT`, `PORTAGE_CONFIGROOT`, `CBUILD` (queried via
`portageq envvar`), and `BUILD_{CFLAGS,CXXFLAGS,CPPFLAGS,LDFLAGS}`. It relies
entirely on step 2 above finding the CHOST-prefixed wrapper on `$PATH` — which
works because either (a) the build is a real chroot (catalyst stage-building:
the ROOT's own `/usr/bin` *is* `/usr/bin` from inside), or (b) it's a genuine
crossdev cross-compiler, whose CHOST-prefixed binary is a host-native
executable installed straight onto the shared host `/usr/bin` (never inside
a sysroot at all).

**em doesn't chroot**, so neither (a) nor (b) holds for a `--root`/`--prefix`
offset — `$PATH` never naturally contains the offset's own `usr/bin`. em's
`shell.rs` compensates for this today, but only for the `chost != cbuild`
case: it explicitly sets `CC`/`CXX`/`AR`/`NM`/`RANLIB`/`STRIP`/`OBJCOPY`/
`OBJDUMP`/`READELF`/`LD` to `${chost}-<tool>` and prepends a
crossdev-sysroot-shaped bin dir (`build_config_root`'s grandparent `/bin`) to
`PATH` — i.e. em already *does* exactly the extra plumbing real portage gets
for free from chrooting, just gated behind "is this genuinely cross" rather
than "is there an offset toolchain directory to prefer regardless of arch".

So (b) isn't a parallel native branch — it's **broadening this existing
block's condition** so it also fires for a plain same-arch `--root <dir>`
offset once `em select` has written that dir's own wrapper (from fix (a)),
using the offset's own `usr/bin` instead of the crossdev-shaped
`build_config_root` grandparent path. The two cases (`--root` native offset,
`--cross`/`--prefix`) should end up sharing one "does this topology have its
own toolchain bin dir, and if so prefer it" check rather than being two
separate code paths keyed on arch difference — the arch difference is
irrelevant to *this* mechanism in real portage too.

**Two smaller gaps found doing this comparison:**
- em's explicit tool list omitted `AS` (assembler) and `PKG_CONFIG` — both
  reasonably common (`AS` for anything with hand-written asm, `PKG_CONFIG` for
  most `configure`/`meson` builds); `CPP`/`F77`/`FC`/`RC`/`DLLWRAP`/`GCJ`/`GO`/
  `HIPCXX` are genuinely rare and fine to keep deferred. **Fixed 2026-07-16**
  (both added to the explicit tool list).
- em never sets `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`/`PKG_CONFIG_PATH`
  itself. **Checked, no fix needed**: real crossdev's `${CTARGET}-pkg-config`
  wrapper (`/usr/bin/cross-pkg-config` on this host, symlinked per target) is
  a shell script that derives `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`
  itself at invocation time from `$ESYSROOT`/`$SYSROOT`/`$ROOT` (whichever is
  set) — the exact same vars em already exports correctly (`portage-repo/src/
  build/shell.rs`'s `ROOT`/`EROOT`/`SYSROOT`/`ESYSROOT`/`BROOT` block). So
  once the wrapper itself is *findable* (the PATH/broot fix below), it needs
  no further help from em.
- No gap found for `BUILD_CC`/`CC_FOR_BUILD`/`HOSTCC`: em sets none of them,
  but doesn't need to — `CBUILD` is already exported correctly, and
  `tc-getBUILD_CC`'s own PATH search for `${CBUILD}-gcc` against the host's
  real (untouched) `$PATH` already works, matching real portage's own
  reliance on that same fallback.

### 2026-07-16, fixed: the PATH-prepend/CC-set condition now keys off `broot`, not `build_config_root`

Implemented the "broaden the condition" plan above — but the correct broadening
turned out to be narrower and more precise than "also handle native `--root`":
the existing `chost != cbuild` block's own bin-dir source was wrong on its own
terms, independent of the (still-deferred) native-offset question. It derived
`prefix_bin` from `build_config_root` (`PORTAGE_CONFIGROOT`) via a
crossdev-sysroot-shaped grandparent-of-`<tuple>` computation — a proxy that
only coincidentally produced the right answer for the one topology
(`--prefix --cross`) it was written for, not a principled "where do
BDEPEND-class build tools live" answer.

The correct source, already established elsewhere in this codebase for
exactly this question, is `Cli::broot()`'s merge root (what `merge/mod.rs`
already computes once per run as `host_roots`, `let host_roots =
globals.broot();`): the real host `/` for a privileged `--root` offset, the
prefix itself for an unprivileged `--prefix` overlay (which cannot write the
real host). `EbuildShell` had no notion of this at all — `set_build_roots`
only ever received `config_root`/`sysroot`/`eprefix`. Fixed by:
- Adding a `build_broot: Option<Utf8PathBuf>` field to `EbuildShell`, set via
  a 4th `set_build_roots` param.
- Threading `broot: Option<&Utf8Path>` through `run_inner`/`build_and_merge`/
  `merge_binpkg`/`run_install_worker`/`run` (`ebuild.rs`), the `WorkerArgs`
  IPC struct + `--broot` worker CLI flag (`privilege.rs`/`cli.rs`), and every
  `merge/mod.rs` call site — each now passes `Some(host_roots.merge_root())`
  (a value merge/mod.rs already had in scope) instead of nothing.
- The toolchain-selection block in `shell.rs`'s `init_build_env` now computes
  `prefix_bin` from `build_broot.join("usr/bin")` directly (no
  grandparent-of-parent arithmetic needed — `broot` is already a root
  directory, unlike the old sysroot-shaped `build_config_root`).

This is a pure correctness fix, not an extension to native `--root`: for a
plain native offset the whole block still doesn't run (`chost == cbuild`
there), so the already-reverted-once native-ROOT-toolchain hazard (see the
2026-07-16 entry above) is untouched. For the cases the block *does* run
(`--cross`, `--cross --prefix`, `--cross --local`), `broot` resolves to
exactly the same directory `build_config_root`'s grandparent trick used to
land on by construction, so no regression — confirmed both by two new unit
tests (`cross_toolchain_selection_uses_broot_not_config_root`,
`cross_toolchain_selection_no_op_when_tool_unreachable`,
`portage-repo/src/build/shell/tests.rs`) and by a live check against this
host's real riscv64 crossdev toolchain (`em --target riscv64-unknown-linux-gnu
ebuild <probe> setup`): `CC`/`CXX`/`AS`/`LD`/`PKG_CONFIG` all resolved
correctly to `/usr/bin/riscv64-unknown-linux-gnu-<tool>`. Full workspace
check/clippy -D warnings/fmt/test suite clean.

## Open: clang linker config (Option B)

`-fuse-ld=` lives in `/etc/clang/<SLOT>/gentoo-linker.cfg`, not env.d. Decide:
fold into `em select linker`, a `em select clang linker` subcommand, or flags on
`em select clang set`. Low priority.
