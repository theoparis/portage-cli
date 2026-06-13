# Root model: `--root`, `--prefix`, and the location variables

`em` operates on **three independent roots** that portage also distinguishes
(PMS + `PORTAGE_CONFIGROOT`). Conflating them is a bug; keeping them orthogonal
is what lets one code path serve host management, offset/stage builds,
unprivileged `.local` overlays, and (eventually) crossdev.

| concept | PMS / portage var | governs | `${X}/…` paths |
|---|---|---|---|
| **config root** | `PORTAGE_CONFIGROOT` | profile, make.conf, USE, CFLAGS | `etc/portage/{make.profile,profile,make.conf}` |
| **base root** | (the planner's "installed" view) | what counts as already installed | `var/db/pkg` |
| **target root** | `ROOT` / `EROOT` | where new files install + the new VDB | `var/db/pkg`, install dest |
| **sysroot** | `SYSROOT` / `ESYSROOT` | where build-time `DEPEND` is found | headers / libs / `.pc` |
| (build tools) | `BROOT` | where `BDEPEND` tools run | always host `/` |

## The two user-facing flags

- **`--root R`** (default `/`, env `ROOT`): the **base**. It is *both* the
  config source *and* the root whose VDB the planner reads as "already
  installed."
- **`--prefix P`** (default `= R`): the **install destination** for newly
  merged packages — the *delta* root.

Two surgical overrides exist: **`--config-root C`** (config only) and
**`--vdb V`** (base VDB path only). All four are global.

### Derived values

```
config_root   = --config-root || --root || /          # PORTAGE_CONFIGROOT
base_root  R  = --root || /                            # planner "installed" base + config
target P      = --prefix || --root || /               # ROOT/EROOT: install dest + new VDB
base_vdb      = --vdb || R/var/db/pkg
planner installed = VDB(R) ∪ VDB(P)                   # P shadows R; equal ⇒ just one
merge into    = P
sysroot search = [P, R]                                # ordered, P wins; equal ⇒ [R]
broot         = /                                      # always host
EPREFIX       = ""                                     # we use ROOT, not Gentoo-Prefix EPREFIX
```

`--prefix` never changes `config_root` or `base_root` — only the destination.
That is the whole difference between "full offset" and "overlay":

- `--root foo/` alone ⇒ `R = P = foo/`: base *and* destination are `foo/`.
- `--prefix foo/` alone ⇒ `R = /`, `P = foo/`: base is the host, destination is
  the prefix.

## Scenarios

| invocation | R (base/config) | P (install) | planner installed | result |
|---|---|---|---|---|
| `em firefox` | / | / | host | normal host install |
| `em --root foo/ firefox` | foo/ | foo/ | VDB(foo/) (empty ⇒ **full closure**) | install *everything* up to firefox into `foo/` — stage / chroot-less full offset |
| `em --root stage1/ @system` | stage1/ | stage1/ | empty | **build a stage from scratch** |
| `em --prefix foo/ firefox` | / | foo/ | host ∪ VDB(foo/) | host is the base; install only the **new** packages into `foo/` — unprivileged `.local` **overlay** |
| `em --prefix a/ --root b/ firefox` | b/ | a/ | VDB(b/) ∪ VDB(a/) | general overlay: base `b/`, delta into `a/`, config from `b/` |
| crossdev (future) | host | target | target + host BDEPEND | `CHOST≠CBUILD`, sysroot = cross sysroot, `BROOT=/` |

`em --root foo/` and `em --prefix foo/` differ in exactly one thing: whether the
host counts as already installed. `--root` ignores it (full closure rebuilt into
`foo/`); `--prefix` keeps it (only the delta lands in `foo/`).

## Planner behaviour

1. "Installed" = `VDB(base_root) ∪ VDB(target)`, target entries shadowing base.
   - full mode (`P == R`): just `VDB(R)` (empty for a fresh stage ⇒ the plan is
     the entire `DEPEND` closure → a bootstrap).
   - overlay mode (`P ≠ R`): host (`R`) satisfies the base so **no full
     bootstrap**; the prefix (`P`) shadows and carries the delta + resume state.
2. Config (profile/make.conf/USE) is read from `config_root`.
3. The plan is the set of packages needed to satisfy the targets that the
   installed view does not already provide; each is merged into `target`.

## Builder behaviour

Per phase (`run_phase`) we set:

```
PORTAGE_CONFIGROOT = config_root                 # host unless --root/--config-root
ROOT = EROOT       = target                       # install destination
SYSROOT = ESYSROOT = base                          # build-against system; SYSROOT
                                                   #   trailing slash stripped, "/"→""
BROOT              = /
EPREFIX            = ""
```

`SYSROOT`/`ESYSROOT` is the **base**, not the target: the build resolves
`DEPEND` against the base system (host for `--prefix`, the offset for `--root`),
with the target layered on top for overlays. When `base == target`
(host, `--root`) `SYSROOT` collapses to `ROOT`. The shell stores config + the
sysroot (when it differs from `ROOT`) via `set_build_roots`; `run_phase`
defaults `SYSROOT = ROOT` when no separate base is given.

The eclasses (already sourced from the host repo) translate these into
build-system specifics — chiefly `multilib_toolchain_setup` pointing
`PKG_CONFIG_{LIBDIR,PATH,SYSTEM_*}` at `ESYSROOT`, plus `econf --with-sysroot`,
`meson`/`cmake` cross-files, etc. **We never enumerate build systems.**

### Overlay support (`target ≠ base`, e.g. `--prefix`)

With `SYSROOT = base`, a package merged into the **target** is not visible to
later builds in the same run (the toolchain/eclasses point at the base). Making
a chain resolve earlier members needs the target layered on top of the base
sysroot. Two ways were considered; the choice is **config-driven now**, with a
zero-config option deferred.

**Rejected — env injection in our code.** Appending the target's `pkgconfig` to
`PKG_CONFIG_PATH` and its include/lib to `CPPFLAGS`/`LDFLAGS` covers pkg-config +
autotools/make (universal conventions), but some build systems locate deps
through their **own** search root — cmake `find_package` config-mode
(`CMAKE_PREFIX_PATH`/`CMAKE_FIND_ROOT_PATH`), some meson `dependency()`
providers. Covering those means our code enumerating per-build-system knobs,
which this design avoids (portage feeds them from eclasses keyed off a single
`ESYSROOT`). So we do **not** do this in code.

**Chosen — config-driven via `bashrc` (today).** We source portage's `bashrc`
hooks per phase (see "bashrc support" below) with the full env available
(`ROOT`, `SYSROOT`, `get_libdir`, …). The **user** wires the overlay there for
whatever build systems they use — `em` ships no build-system knowledge, and the
user completes it without touching our code. Verified: a `liba`→`usea` chain
into one `--prefix` resolves (pkg-config + compile/link) with this `bashrc`:

```bash
# /etc/portage/bashrc — layer an em --prefix target over the base sysroot
if [[ -n ${ROOT} && ${ROOT%/} != "" && ${ROOT%/} != "${SYSROOT%/}" ]]; then
    _ov=${ROOT%/}; _ld=$(get_libdir)
    export PKG_CONFIG_PATH="${_ov}/usr/${_ld}/pkgconfig:${_ov}/usr/share/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"
    export CPPFLAGS="-I${_ov}/usr/include ${CPPFLAGS}"
    export LDFLAGS="-L${_ov}/usr/${_ld} -Wl,-rpath,${_ov}/usr/${_ld} ${LDFLAGS}"
    # add e.g. CMAKE_PREFIX_PATH=${_ov}/usr for cmake find_package, etc.
fi
```

**Deferred — merged sysroot (zero-config).** Merge the target over the base into
one filesystem view (`fuse-overlayfs` / `overlayfs` under a user namespace) and
point **one `ESYSROOT`** at it; the existing eclass machinery then covers every
build system with **zero enumeration and no user bashrc**. This belongs with
**M3 (namespaces/sandbox)** and gives crossdev a real sysroot for free. (Shipping
our own two-root-aware **eclass overlays** is a complementary lever, but the
merged sysroot keeps the "one `ESYSROOT`, eclasses do the rest" invariant.)

**Status:** overlay works today via a user `bashrc`; out of the box (no bashrc)
`SYSROOT = base`, so single packages whose deps are all in the base build into a
target correctly, but a chain needs the bashrc recipe (or the future merged
sysroot). Full closure (`--root`, target == base) is unaffected.

### bashrc support

`em` sources portage's `bashrc` hooks per phase (not PMS; matches
`__source_all_bashrcs`): each profile's `profile.bashrc` in stack order, then
the user's `${PORTAGE_CONFIGROOT}/etc/portage/bashrc`, after the environment is
set up and before the phase function. They see the full env (`ROOT`, `EROOT`,
`SYSROOT`, `ESYSROOT`, `BROOT`, `PORTAGE_CONFIGROOT`, `get_libdir`, the flag
vars). The per-package `/etc/portage/env/` mapping is not yet sourced. This is
the general user hook for env tweaks; the overlay recipe above is one use.

### Known hard part

Native (`CHOST == CBUILD`) discovery of **non-`.pc` headers/libs** under a
`target ≠ /` is the genuine soft spot: a plain host `gcc` won't look in
`target/usr/include` without `--sysroot`/`-I` injection. It is mostly papered
over by pkg-config (`ESYSROOT`) + `econf --with-sysroot`, and the `bashrc`
recipe handles it for `--prefix`. The merged sysroot resolves it generally;
true isolation across arches is crossdev's job (CHOST-prefixed toolchain with a
baked sysroot).

## Orthogonal axis: package source (build vs binpkg)

How a package's *image* is produced — built from source, or unpacked from a
**binary package** (local or third-party) — is independent of the root model.
Once an image exists, the **merge** is identical: walk image → `target`,
register VDB, CONFIG_PROTECT, ecompress/estrip already applied at build/package
time. So binpkg support (`-k/-K`, gpkg/xpak, third-party `BINHOST`) is a
separate axis that reuses the same merge path; it changes only the *producer*,
never the root handling.

## Implementation staging

- **Stage 1 — the three-root split (planner side) [done]:** `Roots`
  (`config`/`base`/`target`); planner config from `config_root`, installed =
  `VDB(R) ∪ VDB(P)`, merge into `target`; applets (`env`, `world`, `query`, …)
  read the right root; `SYSROOT` trailing slash.
- **Stage 1b — the builder side [done]:** thread the roots through
  `build_and_merge`/`run`/`run_inner` to `EbuildShell::set_build_roots`;
  `run_phase` sets `PORTAGE_CONFIGROOT = config`, `ROOT/EROOT = target`,
  `SYSROOT/ESYSROOT = base` (collapsing to `ROOT` when base == target), `BROOT
  = /`; `apply_profile_env` reads config from `config_root`. Makes host, full
  offset / stage, and single-package `--prefix` (deps in base) builds correct.
- **Stage 2 — overlay (`--prefix`, target ≠ base) [config-driven, done]:** `em`
  sources portage `bashrc` hooks per phase exposing the roots + `get_libdir`;
  the user wires overlay search paths there (recipe in "Overlay support"). No
  build-system knowledge in code. The zero-config **merged sysroot** (single
  `ESYSROOT` over a `fuse-overlayfs`/`overlayfs` union) is deferred to M3.
- **Stage 3 — crossdev:** `CBUILD`/`CHOST`/`CTARGET`, decoupled sysroot,
  CHOST-prefixed toolchain, QEMU for tests. Reuses the merged-sysroot work.
- **Orthogonal — binpkg:** producer-only; plugs into the existing merge.
