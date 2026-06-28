# portage-cli

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-cli/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-cli/actions?query=workflow:CI)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-cli/status.svg)](https://deps.rs/repo/github/lu-zero/portage-cli)

A Rust reimplementation of the Gentoo Portage command-line tools, built on a
family of purpose-built crates for parsing atoms, metadata, repositories, and
the installed package database.

> **Note**: For a more mature Rust-based alternative, see
> [Pkgcraft](https://pkgcraft.github.io/).

> **Warning**: This codebase is currently mainly slop-coded and has not yet been
> thoroughly audited, some crates are already polished up to a degree and perform
> correctly. Use at your own risk.

> **Pre-release git checkout**: This is development source from `git` before the
> first release of `portage-cli` / the `em` binary on crates.io. The Applet status
> table below (and the per-crate "Published" vs "Local only" table) documents the
> current implementation state. See the warning above.

## The `em` binary

`em` is a unified front-end for the Portage tool suite. It dispatches to
subcommands corresponding to the traditional tools.

### Applet status

| Applet | Maps to | Status |
|--------|---------|--------|
| `atom` | — | Working |
| `query` | `equery` | Partial — see below |
| `use` | `euse` | Partial — see below |
| `maint` | `emaint` | Partial — see below |
| `regen` | `emerge --regen` | Working |
| `search` | `emerge --search` | Working |
| *(default)* | `emerge` | Working — resolve → build loop, `--prefix` support |
| `ebuild` | `ebuild` | Working — fetch, unpack, phases, merge, VDB registration |
| `depclean` | `emerge --depclean` | Stub |
| `quickpkg` | `quickpkg` | Stub |
| `mirror` | `emirrordist` | Stub |
| `clean` | `eclean` | Stub |
| `revdep` | `revdep-rebuild` | Stub |
| `news` | `eselect news` | Stub |
| `glsa` | `glsa-check` | Stub |
| `log` | `genlop` | Stub |
| `grep` | `egreplite` | Stub |
| `select` | `eselect` | Partial — `profile`, `repository`, `compiler`, `binutils`, `linker`, `clang` |
| `crossdev` | `crossdev` | Working — cross sysroot/overlay setup + staged toolchain bootstrap |
| `toolchain` | — | Working — native self-hosting toolchain bootstrap into `--root` |
| `dispatch` | `dispatch-conf` | Stub |
| `etc` | `etc-update` | Stub |
| `env` | `env-update` | Stub |

---

### `em query` (equery)

| Subcommand | Alias | Status |
|---|---|---|
| `belongs` | `b` | Working — file → owning package via VDB CONTENTS |
| `check` | `k` | Working — MD5 checksum + mtime verification |
| `depends` | `d` | Working — reverse-dep search in metadata cache |
| `depgraph` | `g` | Working — full dep tree via PubGrub solver, portage-compatible output |
| `files` | `f` | Working — all files installed by a package |
| `has` | `a` | Working — VDB field search across installed packages |
| `hasuse` | `h` | Working — packages with a given USE flag in IUSE |
| `keywords` | `y` | Working — keyword status across architectures |
| `list` | `l` | Working — available packages; `-I` for installed only |
| `meta` | `m` | Working — maintainers, homepage, longdesc, installed info |
| `size` | `s` | Working — installed size + build timestamp |
| `uses` | `u` | Working — IUSE flags with descriptions + installed status |
| `which` | `w` | Working — path to best matching ebuild |

**`em query depgraph` feature summary:**

- **VDB awareness** — installed packages are registered with `InstalledPolicy::Favor`; already-installed exact CPVs are filtered from output; build-time deps (DEPEND/BDEPEND) are skipped for installed packages (already built)
- **Profile USE flags** — `make.defaults` files are sourced through brush with per-layer isolation (each file's USE assignments are its pure delta, merged with portage-style incremental semantics); `make.conf` receives the same treatment so bare `USE="…"` in make.conf correctly *adds* flags rather than replacing the profile's defaults
- **USE_EXPAND** — `PYTHON_TARGETS`, `CPU_FLAGS_ARM`, `ABI_X86`, etc. are expanded into flag tokens and grouped in output (e.g. `PYTHON_TARGETS="python3_13 python3_14"`)
- **OR-group branch selection** — selects the branch whose USE dep constraints are already satisfied by the installed state and current USE config (avoids unnecessary rebuilds while respecting profile-mandated targets)
- **Post-solve reinstall detection** — after solving, installed packages whose USE dep constraints are violated by the resolved set are flagged `R` (rebuild with changed USE), matching portage's basic `-p` output
- **Action tags** — `N` new, `NS` new slot (alongside existing slots), `U` upgrade (with `[old_ver]`), `D` downgrade, `R` reinstall; slot-aware
- **Profile + user `package.use`** — full profile stack `package.use` and `/etc/portage/package.use` loaded and applied per-package to the solver; USE dep violations on new packages show the intended (post-install) state rather than the absent current state
- **Cycle handling** — BDEPEND bootstrap cycles (e.g. `xz-utils` ↔ `elt-patches`) are broken after Kahn's topological sort rather than silently dropping packages

**Performance** (arm64, warm file cache):

| Target | `emerge -p` | `em query depgraph` |
|--------|------------:|--------------------:|
| `www-client/firefox` | 3.6 s | **0.88 s** |
| `app-text/texlive` | 2.3 s | **0.89 s** |
| `dev-lang/rust` | 1.8 s | **0.90 s** |
| `sys-devel/gcc` | 1.6 s | **0.91 s** |

Metadata cache entries are parsed in parallel (jwalk + chunked `spawn_blocking`). The PubGrub solver itself runs in 5–35 ms depending on solution size.

**Gaps vs `emerge -p`:**
- Global USE consistency propagation (portage's `--newuse` full scan) is not implemented; constraint-driven reinstall detection covers the same cases as basic `emerge -p`
- Wrapper packages for old-slot BDEPEND (`autoconf-wrapper`, `gcc-config`, etc.) are not modelled
- Flag ordering differs (em is alphabetical; portage groups enabled flags first)
- Upgrade display shows all flags rather than only the changed ones
- `(-flag)` parentheses for USE_EXPAND_IMPLICIT / arch-forced-off flags not yet rendered

**Gaps vs equery:**
- `uses` descriptions come from `profiles/use.desc` + `profiles/use.local.desc`.
  Overlay packages not yet regen'd fall back to empty description (metadata.xml
  per-package lookup is not yet wired as a fallback).
- No `stats` subcommand.

---

### `em use` (euse)

| Flag | Status |
|---|---|
| `-a FLAG` | Working — add USE flag to `make.conf` |
| `-r FLAG` | Working — remove USE flag from `make.conf` |
| *(no flags)* | Working — print current USE value |
| `--make-conf PATH` | Working — override make.conf path |

**Gaps vs euse:**
- No `-p pkg` for package-specific USE flags (`/etc/portage/package.use`).
- `get()` returns the raw unexpanded value; `${COMMON_FLAGS}` references are
  not evaluated (brush-backed expansion is possible but not wired yet).

---

### `em maint` (emaint)

| Subcommand | Status | Notes |
|---|---|---|
| `world` | Working | Checks `world` + `world_sets`; validates `@set` refs against known sets from `/usr/share/portage/config/sets/`, `/etc/portage/sets.conf`, and `/etc/portage/sets/`; `--fix` rewrites both files |
| `revisions` | Working | Purges `repo_revisions` JSON (sync commit history); optional per-repo targeting |
| `moveinst` | Partial | Detects packages needing rename from `profiles/updates/`; does not apply moves or scan installed dependency metadata |
| `regen` | Working | Available as `em regen` |

**Gaps vs emaint:**

- `moveinst` — missing the second pass that walks every installed package's
  `DEPEND`/`RDEPEND`/etc. fields for stale atom references, and the `--fix`
  mode that writes to the VDB.
- `world` — `@set` references are validated by name but not by content (e.g.
  `@preserved-rebuild` is accepted as long as the name is known).
- `all`, `binhost`, `cleanconfmem`, `cleanresume`, `logs`, `merges`,
  `movebin`, `sync` — not implemented.

---

## Cross-compilation & toolchains

`em` understands the multi-root model (`docs/root-model.md`): a build reads its
config from one root (`--config-root`) and installs into another (`--root`),
with build tools resolved against the host (`BROOT`). On top of that it can
bootstrap toolchains and assemble stages.

- **`em crossdev -t <tuple> --init-target`** lays down a cross sysroot + overlay
  (a `crossdev` workalike); **`--setup`** then runs the staged
  `binutils → headers → gcc-stage1 → libc → gcc-stage2` bootstrap into
  `/usr/<tuple>`. Validated end-to-end for `riscv64-unknown-linux-gnu`.
- **`em toolchain --setup --root <dir>`** bootstraps a *native* self-hosting
  toolchain (`CHOST == CBUILD`) into an empty root —
  `baselayout → binutils → os-headers → glibc → gcc`. Unlike cross there is no
  two-stage gcc: the host (seed) compiler builds full glibc directly and a single
  full gcc links against it. Verified: a fully automated run produces a
  `gcc-16.1` in the root that compiles and links a working binary against the
  root's own libc.

The native toolchain and the cross bootstrap share one staged driver
(`crossdev::stages`), differing only in atom naming and how the `glibc ↔ gcc`
cycle is broken. Stage *production* (stage1 `packages.build`, stage3
`--emptytree @system`) is the next layer — see `todo/em-stages-and-binhosts.md`.

---

## Architecture

See [`docs/architecture.md`](./docs/architecture.md) for the full crate
dependency graph, per-crate API catalog, and design reference.

### Crate family

| Crate | Purpose | Status |
|-------|---------|--------|
| `portage-atom` | PMS atom parser (`Cpn`, `Cpv`, `Dep`, `Version`) | Published |
| `portage-metadata` | md5-cache entry parser, EAPI, phases, keywords | Published |
| `portage-repo` | Repo layout, profiles, metadata cache, ebuild sourcing | Local only |
| `portage-vdb` | Installed package database reader (`/var/db/pkg`) | Published |
| `portage-atom-resolvo` | SAT dependency solver (resolvo bridge) | Published |
| `portage-atom-pubgrub` | Alternative solver (PubGrub bridge) | Published |
| `gentoo-core` | Architecture types | Published |
| `gentoo-stages` | Stage3 tarball fetch/cache | Published |

### brush integration

`portage-repo` embeds [brush](https://github.com/lu-zero/brush) (the
`for-portage-repo` fork branch) — a Rust bash interpreter — for ebuild
sourcing and `make.conf` parsing. Additions to the fork:

- `Program.comments: Vec<SourceSpan>` — comment spans from the winnow parser,
  used by `MakeConf` for byte-precise round-trip editing.
- `ParseContext.comments` accumulator and comment-tracking whitespace parsers
  (`spaces_tracking`, `linebreak_tracking`, `newline_list_tracking`).

## Installation

```bash
cargo install --path portage-cli
```

## Local Development

The project expects sibling checkouts:

```
portage-cli/    # this workspace
brush/          # brush fork at for-portage-repo branch
```

The `.cargo/config.toml` at workspace root patches the brush crates to use the
local checkout.

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](./AGENTS.md) for project conventions (Conventional Commits,
style, checks).

## Author

Luca Barbato <lu_zero@gentoo.org>
