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

## Open: clang linker config (Option B)

`-fuse-ld=` lives in `/etc/clang/<SLOT>/gentoo-linker.cfg`, not env.d. Decide:
fold into `em select linker`, a `em select clang linker` subcommand, or flags on
`em select clang set`. Low priority.
