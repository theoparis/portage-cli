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
PORTAGE_CONFIGROOT = config_root
ROOT = EROOT       = target
SYSROOT = ESYSROOT = target            # primary; trailing slash stripped, "/"→""
BROOT              = /
EPREFIX            = ""
```

The eclasses (already sourced from the host repo) translate these into
build-system specifics — chiefly `multilib_toolchain_setup` pointing
`PKG_CONFIG_{LIBDIR,PATH,SYSTEM_*}` at `ESYSROOT`, plus `econf --with-sysroot`,
`meson`/`cmake` cross-files, etc. **We never enumerate build systems.**

### The ordered-sysroot subtlety (`P ≠ R`, overlay)

PMS `SYSROOT` is a *single* path, but overlay needs `[P, R]`. We handle it by:
- setting `ESYSROOT = P` (eclasses point pkg-config at the prefix), and
- **augmenting** the search with `R`: append `R`'s `pkgconfig` dirs to
  `PKG_CONFIG_PATH`, and `R`'s include/lib to the compiler search.

When `R = /` (the common overlay) this is cheap: the host toolchain already
searches `/usr/include` and `/usr/lib*` natively, so we only need to *add* `P`.
When `R ≠ /` (`--prefix a/ --root b/`) we must inject both — the advanced case.

### Known hard part

Native (`CHOST == CBUILD`) discovery of **non-`.pc` headers/libs** under a
`target ≠ /` is the genuine soft spot: a plain host `gcc` won't look in
`target/usr/include` without `--sysroot`/`-I` injection. It is mostly papered
over by pkg-config (`ESYSROOT`) + `econf --with-sysroot`, and disappears
entirely for `R = /` overlays. True isolation across arches is crossdev's job
(CHOST-prefixed toolchain with a baked sysroot).

## Orthogonal axis: package source (build vs binpkg)

How a package's *image* is produced — built from source, or unpacked from a
**binary package** (local or third-party) — is independent of the root model.
Once an image exists, the **merge** is identical: walk image → `target`,
register VDB, CONFIG_PROTECT, ecompress/estrip already applied at build/package
time. So binpkg support (`-k/-K`, gpkg/xpak, third-party `BINHOST`) is a
separate axis that reuses the same merge path; it changes only the *producer*,
never the root handling.

## Implementation staging

- **Stage 1 — the three-root split (foundation, all scenarios need it):**
  `config_root()` / `base_root()` / `target()` accessors; planner config from
  `config_root`, planner installed = `VDB(R) ∪ VDB(P)`, merge into `target`;
  applets (`env`, `world`, `query`, …) read the right root; `SYSROOT` trailing
  slash. Makes host (1), full offset / stage (2,3) correct.
- **Stage 2 — overlay (`--prefix`, `P ≠ R`):** union planner view (done in
  Stage 1) + ordered sysroot augmentation (`PKG_CONFIG_PATH`/include/lib add
  `R`) + runtime `rpath`/`LD_LIBRARY_PATH` so `.local` wins at run time.
- **Stage 3 — builder config offset:** `apply_profile_env(config_root)` +
  `PORTAGE_CONFIGROOT`, so `--root` *builds* (not just plans) against the
  offset's config. Needed for true stage/chroot-less offset builds.
- **Stage 4 — crossdev:** `CBUILD`/`CHOST`/`CTARGET`, decoupled sysroot,
  CHOST-prefixed toolchain, QEMU for tests.
- **Orthogonal — binpkg:** producer-only; plugs into the existing merge.
