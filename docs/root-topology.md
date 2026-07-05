# Root topology: scenarios, variants, and the satisfaction-root map

This is the design reference for how `em` models the filesystem locations a
Gentoo build touches. It supersedes the scenario narrative in
[`root-model.md`](./root-model.md) (which stays as the historical, builder-side
detail reference). Read this first; cross-link into `root-model.md` only for
the `bashrc`/overlay recipe and the per-phase env (`SYSROOT`/`ESYSROOT`/`BROOT`
assignment in `run_phase`).

> **Slop warning.** Verify any claim here against the code before relying on
> it. The current `Roots` struct (`portage-cli/src/cli.rs`) is the
> single-root-bag shape this doc argues we should move *away* from; the
> variant enum below is the proposed target, not the present reality.

## The four roles

A Gentoo build touches up to four distinct filesystem locations. PMS fixes the
*meaning* of each; only the *paths* vary per invocation.

| role | PMS / portage var | governs |
|---|---|---|
| **base root** | (planner "installed" view) | what counts as already installed — seeds the plan |
| **target root** | `ROOT` / `EROOT` | install destination, the new VDB |
| **sysroot** | `SYSROOT` / `ESYSROOT` | build-against headers/libs/`.pc` for `DEPEND` |
| **BROOT** | `BROOT` | where `BDEPEND` tools run (host machine, native `${CBUILD}`) |

**Config is orthogonal to the roles** — it is not a fifth role, it is a single
global path: `PORTAGE_CONFIGROOT`, the profile + `make.conf` source, defaulting
to `/`. `--config-root` overrides it; `--root` does **not** (matching portage's
`ROOT=R emerge`, which changes only the install destination, not the profile).
A separate `config_overlay` (`--prefix`) layers per-user
`package.use`/`package.keywords`/`package.license`/`bashrc` on top of the
profile, never replacing it. (`--local` is self-contained — config lives in
the prefix itself, not overlaid on the host's.)

This matters because in a cross build the host's config and the target's config
genuinely differ: the sysroot's `make.conf` pins `CHOST`/`CBUILD` and carries
target USE flags, while BROOT's carries the host's. A Host-BDEPEND package
(jinja2 built for host python) builds against BROOT's config, not the target
sysroot's. The current single-config `Roots` field cannot express this; the
model below can, because cross points `config` at the sysroot (where crossdev
wrote the target profile) while BROOT's own `etc/portage` remains the host's.

## Override semantics

Each user-facing flag maps to a portage knob and overrides exactly what that
knob overrides — no more, no less:

| flag | target (ROOT) | base (planner VDB) | config (profile source) | `package.*` overlay | EPREFIX |
|---|---|---|---|---|---|
| *(none)* | `/` | `/` | `/` | — | — |
| `--root R` | R | R | **`/`** *(unchanged)* | — | — |
| `--config-root C` | `/` | `/` | C | — | — |
| `--root R --config-root C` | R | R | C | — | — |
| `--prefix P` | P | **`/`** *(host seeds plan)* | `/` | P/etc/portage | P |
| `--local` | ~/.gentoo | **~/.gentoo** *(self-contained)* | ~/.gentoo/etc/portage | ~/.gentoo | ~/.gentoo |
| `--cross T` (on EROOT) | EROOT/usr/T | EROOT/usr/T | EROOT/usr/T | — | — |

- **`--root` vs `--prefix` differ in two cells: base and EPREFIX.** `--root`
  moves base to R (empty R → full closure → stage build) and leaves EPREFIX
  unset (installed scripts use host-absolute paths); `--prefix` keeps base at
  `/` (host seeds the plan → only the delta lands in P) and sets EPREFIX=P
  (installed tree under P is relocatable — scripts shebang to
  `${EPREFIX}/usr/bin/...`). Both leave config at the host. **Current code
  diverges:** `cli.rs` makes config follow `--root` (`config: config_root.or(root)`);
  portage parity requires config to stay at `/`. That is a real divergence to fix
  as part of this refactor.
- **`--local` is the standalone unprivileged deployment** — a self-contained
  Gentoo-Prefix at `~/.gentoo`: base = target = `~/.gentoo` (full closure, not an
  overlay), EPREFIX = `~/.gentoo`, config from `~/.gentoo/etc/portage` (where
  `em setup` places the profile). Unlike `--prefix`, it does **not** assume the
  host is Gentoo — the prefix carries its own VDB, config, and (after
  `--setup`) its own toolchain. This is what makes it work on a foreign host
  (Debian/Arch/Fedora) and what makes `--local --cross <T>` build a real BROOT
  into `~/.gentoo` rather than reading a nonexistent `/`.
  - **`--prefix` vs `--local`** mirror the `--root` vs `--prefix` distinction:
  `--prefix P` is the overlay (host stays base, delta only — fast path on a
  Gentoo host); `--local` is standalone (full closure, self-contained — works
  anywhere). Current code makes `--local` an overlay (base=`/`); the refactor
  makes it standalone to match its actual purpose.
- **`--cross` points config at the sysroot** because crossdev physically writes
  the target profile + `make.conf` there; the host's `etc/portage` remains
  BROOT's config.

The dep-class → role map is fixed by PMS table 8.2:

| dep class | runs on / resolved against |
|---|---|
| `BDEPEND` | **BROOT** (always the build host, native `${CBUILD}`) |
| `DEPEND` | **sysroot** (`ESYSROOT`) |
| `RDEPEND` | **target root** (`ROOT`) |
| `IDEPEND` | running root (BROOT for cross, target for native) |

Getting a build right is, mechanically, getting this map right. Almost every
hard bug in the cross/stage work has been one role silently standing in for
another (see [`stage-build-shakeout.md`](../todo/stage-build-shakeout.md)
#17 CTARGET leak, #18 CHOST invisible, #28–#33 Host/Target root conflation,
#29 build-machine pkgconfig searching the target sysroot).

## The variant enum (target design)

Today `Roots` is a flat bag of five `Option<PathBuf>` fields, and every caller
has to *know* which field answers which role for the current invocation. That
is the structural debt the cross/stage session exposed: `host_roots` /
`base_roots()` is threaded positionally across 9 files precisely because no
type tells you "this is the host-side root for an unsatisfied BDEPEND."

The proposed shape makes the variant answer `satisfaction_root(dep_class)` as
a pure function, so no caller holds an ambiguous `&Roots`. Config and its
overlay are sibling globals (defaulting to `/` and `None`); the variant is
only about the four filesystem roles:

```rust
struct RootTopology {
    /// `PORTAGE_CONFIGROOT` — profile + make.conf source. Defaults to `/`.
    /// `--config-root` overrides; `--root` does NOT (portage `ROOT=` parity).
    config: PathBuf,
    /// Per-user `package.*`/`bashrc` overlay (`--prefix`). Layered
    /// on top of `config`, never replaces the profile. `None` otherwise.
    config_overlay: Option<PathBuf>,
    /// The four filesystem roles, collapsed by how many coincide.
    roots: RootSet,
    /// Same-arch vs foreign-triple (CHOST/CBUILD/CTARGET). Orthogonal to the
    /// topology: cross is the same root routing with different compiler
    /// prefixes, not a fourth variant.
    cross: CrossArch,
}

enum RootSet {
    /// All four roles collapse to one path.
    /// `em <atom>` as root.
    Single { root: PathBuf },
    /// BROOT (build host) distinct from target. Sysroot == target.
    /// `--root R` (BROOT=/), `--cross` (BROOT=outer EROOT).
    Dual { broot: PathBuf, target: PathBuf },
    /// BROOT, base (sysroot source), and target all distinct.
    /// `--prefix P` (base=/, target=P, BROOT=/).
    Overlayed { broot: PathBuf, base: PathBuf, target: PathBuf },
}
```

The `cross: CrossArch` field (`SameArch` / `ForeignArch` with
`CHOST`/`CBUILD`/`CTARGET`) is orthogonal to `RootSet` because **cross is not
a fourth topology** — it's the same root routing with different triples. The
session's `cross_active` + `is_cross_arch` split (`root_aware.rs:66-72`)
already discovered this empirically: routing is identical, only the compiler
prefixes differ.

### What `satisfaction_root` returns, per variant

| dep class | `Single` | `Dual` | `Overlayed` |
|---|---|---|---|
| `BDEPEND` | root | broot | broot |
| `IDEPEND` | root | broot (cross) / target (native) | broot |
| `DEPEND` | root | target | base (sysroot) |
| `RDEPEND` | root | target | target |

`Single` collapses everything; `Dual` splits BROOT from target; `Overlayed`
adds the base/sysroot distinction. Cross vs native only flips the `IDEPEND`
cell (running root) — the one place `satisfaction_root` needs the `cross`
field rather than the `RootSet` alone, so the signature is
`satisfaction_root(&self, class: DepClass) -> &Path` with `self.cross` read
internally for `IDEPEND`.

## The two axes that determine difficulty

The variant captures **axis 1 — how many distinct roots**. But the scenarios
below show a second, mostly-orthogonal axis that determines how *hard* a build
is: **axis 2 — what BROOT is**.

| | BROOT = `/` (rw) | BROOT = `/` (ro, Gentoo host) | BROOT = prefix subset |
|---|---|---|---|
| **native stage1** | trivial | trivial | Tier 3 bootstrap |
| **cross stage1** | crossdev classic | — | layered on Tier 3 |
| **cross stage4** | + big closure | — | + big closure |

Axis 2 is what *privilege* really controls: root buys "BROOT can be the real
`/`"; unprivileged on a Gentoo host buys "BROOT reads `/`"; unprivileged on a
foreign host forces "BROOT must be bootstrapped into writable space."
**BROOT identity should not be a variant field** — it's a property of *what's
installed at BROOT* (is the host VDB present? are the tools there?),
discovered at runtime, not a structural property of the topology. Mixing it in
would conflate "where do roots point" with "is BROOT self-hosting" (the Tier 3
question, which deserves its own modelling in
[`build-environment.md`](./build-environment.md)).

## The five scenarios

Notation: `C` config, `B` base, `T` target, `S` sysroot (ESYSROOT), `BR` BROOT.
"stage1" is overloaded — see "Two meanings of stage1" below.

### 1. Native stage1, privileged (root)

The seed toolchain is the host's own `/usr/bin/gcc`. BROOT is the real `/`,
read+write.

```
C=/  B=T=<offset>  S=T  BR=/        CBUILD==CHOST
```

- `em --root /var/tmp/stage1 toolchain --setup` (builds binutils/glibc/gcc into
  the offset, single-pass since `CHOST==CBUILD`), then
  `em --root /var/tmp/stage1 stages --stage1` (the `packages.build` set).
- BROOT is the real host `/`; every BDEPEND edge is host-satisfied and dropped.
- Topology: **`Dual { broot: /, target: <offset> }`** + `SameArch`. (`Single` is only
  the bare `em <atom>` case where every role is `/`; any offset splits BROOT from
  target.) Config stays at `/` — portage `ROOT=` parity.

### 2. Native stage1, unprivileged

Two genuinely different sub-cases, split by **whether the host `/` already has
the build tools**:

**(2a) Gentoo host, unprivileged user.** `/` is read-only but present and
complete (real portage VDB, real `/usr/bin/cmake`).

```
C=/ (ro)  B=T=<offset>  S=T  BR=/ (ro)    CBUILD==CHOST
```

- Same topology as (1) — **`Dual { broot: /, target: <offset> }`**; the only
  difference is we can't *write* `/`, but we never need to — BDEPEND is satisfied
  by *reading* the host VDB + host binaries. `em --root /var/tmp/stage1 stages
  --stage1` works unchanged.
- **This is just (1) minus root.** For a delta-only deployment into `~/.gentoo`
  on a Gentoo host, use `em --prefix ~/.gentoo` (overlay: host stays base).
  For a self-contained deployment, use `em --local` (see 2b).
- Caveat: a BDEPEND the host lacks (e.g. jinja2 for a python target the host's
  jinja2 doesn't cover) has nowhere to land under pure `--root`/`--prefix` —
  see (2b)/(4).

**(2b) `--local`: self-contained deployment (any host).** The prefix at
`~/.gentoo` is standalone — base = target = `~/.gentoo`, carrying its own VDB,
config, and (after bootstrap) its own toolchain. Works on a Gentoo host *and*
on a foreign host (Debian/Arch/Fedora).

```
C=~/.gentoo/etc/portage  B=T=~/.gentoo  S=~/.gentoo  EPREFIX=~/.gentoo   CBUILD==CHOST
```

- `em setup` bootstraps the initial layout: places `make.profile` + minimal
  `make.conf` into `~/.gentoo/etc/portage`. On a Gentoo host the profile
  symlinks into `/var/db/repos/gentoo`; on a foreign host the user provides
  one (or `--setup` fetches a minimal tree).
- BROOT starts as `/` (host tools compile the first packages) and converges to
  `~/.gentoo` once the prefix toolchain exists — axis 2 (runtime BROOT
  identity), not a topology field.
- Topology: **`Single { root: ~/.gentoo }`** (all roles collapse to the
  prefix once bootstrapped). This is what makes `--local --cross <T>` work on
  a foreign host: BROOT = `~/.gentoo` (writable, real), target =
  `~/.gentoo/usr/<T>`.
- root-model.md's **Tier 3** for the initial bootstrap phase (mutable BROOT,
  hardest case); converges to standalone `Single` once self-hosting.

**`--local` vs `--prefix` at a glance:** `--prefix P` is the overlay (base=`/`,
host seeds plan, delta only — fast path on a Gentoo host, useless on a foreign
one). `--local` is standalone (base=target=`~/.gentoo`, full closure, works
anywhere). They are the `--root`/`--prefix` distinction specialized to the
unprivileged home-directory case: `--local` adds EPREFIX + self-contained config.

### 3. Cross stage1, privileged (root)

Crossdev's classic flow, into `/usr/<CTARGET>`.

```
C=B=T=/usr/<T>  S=/usr/<T>  BR=/     CBUILD≠CHOST
```

- `em --cross <tuple> toolchain --setup` → binutils → headers → libc-headers
  (`--nodeps`) → gcc-stage1 → libc → gcc-stage2. Atoms live under the
  `cross-<tuple>/` overlay; the real `::gentoo` ebuilds are symlinked in.
- BROOT is the real host `/` (native cmake/perl/python). Every BDEPEND edge
  resolves against the host VDB.
- Topology: **`Dual { broot: /, target: /usr/<T> }`** + `ForeignArch`.
- Result: a cross-toolchain (`<T>-gcc`, `<T>-ld`, …) plus target glibc/headers
  in `/usr/<T>`, ready to compile target code.

### 4. Cross stage1, unprivileged

Can't write `/usr/<T>`. Whole sysroot goes under a writable offset.

```
C=B=T=<offset>/usr/<T>  S=<offset>/usr/<T>  BR=<offset>     CBUILD≠CHOST
```

- BROOT is **not `/`** — it's the offset's own native toolchain, i.e.
  **`em --local` (scenario 2b) ran first** to produce a host stage1 at the
  offset, then cross is layered on top targeting `<offset>/usr/<T>`.
  On a Gentoo host, `--prefix <offset>` (2a overlay) also works — BROOT reads
  `/` directly.
- This is *exactly* the session's `/var/tmp/cross-stage1-riscv64`:
  `base_roots()` = the outer EROOT (host stage1, the BROOT), `--cross` targets
  the sysroot subdir. The jinja2/perl/Host-BDEPEND routing bugs (#28–#33) were
  all about BDEPEND edges landing in `base_roots()` instead of `/` or the
  sysroot.
- If the host *is* Gentoo and complete, (2a) applies and BR can read `/` — but
  the session deliberately kept it self-contained under the offset to avoid
  depending on the real machine.
- Topology: **`Dual { broot: <offset>, target: <offset>/usr/<T> }`** +
  `ForeignArch`. (BROOT being a prefix subset rather than `/` is axis 2, not a
  topology difference from (3).)

### 5. Cross stage4 (full target system)

A bootable/installable target system — a real `<T>` stage3+ that boots on
`<T>` hardware. Same topology as (3) or (4) (whichever privilege tier); stage4
just means the *closure* is `@system` + a custom set rather than the
toolchain.

**Inputs:**
1. A working **cross-toolchain** (output of 3 or 4): `<T>-gcc`, target glibc +
   headers in the sysroot.
2. The **target sysroot seeded** with libc + a minimal VDB.
3. Build **`@system` (stage3) + custom set (stage4)** as *target-native*
   packages: each has `CHOST==CTARGET==<T>`, `CBUILD==host`, installs into the
   target root, records in the target VDB.

**The two real hazards (both already worked through in the session):**

- **BDEPEND visibility into BROOT.** A target-native package's BDEPEND (e.g.
  `dev-python/jinja2` for `systemd-utils`) runs on BROOT under the *host's*
  python — must be installed for the host python target, not the target one.
  Unsatisfied BDEPEND must schedule a **`MergeRoot::Host` merge** into BROOT,
  not into the target sysroot. This is the #28/#30/#31/#32 bug class, all
  fixed.
- **Genuine bootstrap SCCs.** `gawk → bison → gettext → libxml2 → meson →
  python → gawk` is a real strongly-connected component with no valid linear
  order. Broken by seeding one member (`--nodeps`), exactly as catalyst/portage
  do for `xz-utils ↔ elt-patches`. Not a bug; an inherent property of
  bootstrapping a self-hosting set.

**Not a hazard, despite prior claims:** "some ebuilds just can't
cross-build." Every such case in the session turned out to be a
misdiagnosed env-var bug (build-machine pkgconfig searching the target sysroot
— #29, fixed by `BUILD_PKG_CONFIG_LIBDIR` → outer EROOT in `de87153`; CTARGET
leak — #17; CHOST invisible to subprocesses — #18). Real cross builds a full
target-native stage3 *without ever executing a target binary on the host* —
the build phase runs the host compiler producing target binaries that don't
run until installed on target hardware. `qemu-user` is at most a per-ebuild
escape hatch for upstream bugs that execute helpers at build time (some
`src_test`, broken ebuilds); it is **never** an architectural stage4
dependency, and `crossdev-stages` (separate tool,
`/home/lu_zero/Sources/crossdev-stages`) is the proof — it produces target
stage3 sandboxes with no qemu involvement.

## Lifecycle: setting up each topology

A root rarely starts empty and usable. `em setup` (layout bootstrap) and
`em toolchain --setup` (compiler bootstrap) are the two lifecycle primitives;
cross adds `em crossdev --init-target` (sysroot config). What each does depends
on the topology being bootstrapped.

### `em setup` — layout bootstrap

Creates the directory skeleton, `make.conf`, `bashrc`, and (for self-contained
roots) `repos.conf` + `make.profile`. Implemented in
[`setup.rs`](../portage-cli/src/setup.rs); never touches `/`.

| target | what `em setup` writes |
|---|---|
| `--prefix P` (overlay) | skeleton + a `make.conf`/`bashrc` **overlay** (host profile + make.conf stay authoritative; `bashrc` injects `-I$P/usr/include` etc. so the compiler sees the delta) + **host-python/host-tool symlinks** into `P/usr/bin` (the installed tree is relocatable under EPREFIX=P, so ebuilds bake `${EPREFIX}/usr/bin/pythonX.Y` into shebangs; since the overlay borrows the host's python rather than building one, the symlink satisfies those shebangs) |
| `--local` (standalone) | skeleton + **self-contained** `make.conf`/`bashrc` under `~/.gentoo/etc/portage`. Builds its **own** python via `toolchain --setup`; during bootstrap the host's python is reached via PATH/BROOT, never via a symlink masquerading as a prefix-owned file |
| `--root R` (self-contained offset) | skeleton + self-contained `make.conf` (with real `MAKEOPTS`/`ACCEPT_KEYWORDS` — this is the *only* make.conf it reads) + `repos.conf` + `make.profile` symlinked to the host's resolved profile |

The `bashrc` distinction is load-bearing
([`setup.rs:131-157`](../portage-cli/src/setup.rs)): an overlay (`--prefix`,
`--local`-as-overlay) needs CPPFLAGS/LDFLAGS injection so the compiler sees the
delta layered over the host; a self-contained root (`--root`, `--local`-as-
standalone) must **not** get that injection — it actively breaks builds by
shadowing a package's own version-matched headers with the root's libc
(`gcc libiberty/obstack.c` vs the ROOT's `obstack.h`, found 2026-07-03).

### Plain unprivileged toolchain (`em toolchain --setup`)

Builds a native `baselayout → binutils → os-headers → glibc → gcc` into `--root`
(`BootstrapKind::Native`, single-pass since `CHOST==CBUILD`). The compiler this
produces is what `em stages --stage1` then builds `packages.build` against.

```
em --root /var/tmp/stage1 toolchain --setup
em --root /var/tmp/stage1 stages --stage1
```

`toolchain --setup` calls `ensure_self_contained_prefix` first
([`crossdev/mod.rs:710`](../portage-cli/src/crossdev/mod.rs)) — runs `em setup`
if the root is non-`/`, writes `repos.conf`/`make.profile` — so it is
self-sufficient: a fresh empty `--root` becomes a buildable toolchain in one
command. Requires `--root <dir>`; a toolchain into `/` is meaningless (use the
host's own).

### `--local` and `--prefix` setup

These don't run `toolchain --setup` themselves — they assume the host (or, for
`--local` after bootstrap, the prefix) provides a compiler. The lifecycle:

```
# --prefix (overlay on a Gentoo host): host provides everything
em --prefix /opt/prefix setup          # layout + overlay config + host-python symlinks
em --prefix /opt/prefix <pkg>          # host compiler builds into P

# --local (standalone): bootstrap the prefix's own toolchain first
em --local setup                       # layout + self-contained config (own python later)
em --local toolchain --setup           # build native toolchain INTO ~/.gentoo
em --local stages --stage1             # packages.build using the prefix's own gcc
em --local <pkg>                       # now self-hosting
```

The `--local` case is where the standalone-vs-overlay decision bites: under the
current (overlay) code, `em --local toolchain --setup` reads base from `/`,
works only on a Gentoo host, and (wrongly) symlinks host python into the prefix
— `setup.rs` gates `link_host_pythons`/`link_host_base_tools` on `is_local`,
exactly backwards. Under the proposed model the symlinks move to `--prefix`
(the overlay that borrows host tools), and `--local` builds its own python so
base is `~/.gentoo` from the start — the toolchain bootstrap lands in the
prefix and works on a foreign host too, at the cost of needing the host's seed
compiler on `PATH` for the very first build (the same chicken-and-egg every
Gentoo-Prefix bootstrap faces).

### Cross setup (`em crossdev`)

Cross needs three things the native cases don't: the `cross-<tuple>` overlay,
the sysroot's `make.conf` (pinning `CHOST`/`CBUILD`/`CTARGET`), and the
two-stage gcc bootstrap. Implemented in
[`crossdev/mod.rs`](../portage-cli/src/crossdev/mod.rs):

```
# Privileged: classic crossdev into /usr/<T>
em crossdev -t <tuple> --init-target     # overlay + sysroot make.conf/profile
em crossdev -t <tuple> --setup            # binutils→headers→gcc1→libc→gcc2 (implies --init-target)
em --cross <tuple> stages --stage1       # target packages.build
em --cross <tuple> --emptytree @system   # stage3 (target-native @system)

# Unprivileged: same, under --local (or --root <offset>)
em --local crossdev -t <tuple> --init-target
em --local crossdev -t <tuple> --setup
em --local --cross <tuple> stages --stage1
...
```

`--init-target` writes the sysroot `etc/portage/{make.conf,make.profile}` via
`write_sysroot_config` (the `make.conf` that pins the triples and sets
`PKG_CONFIG_*`/`BUILD_PKG_CONFIG_LIBDIR` — the latter being the #29 fix).
`--setup` runs `BootstrapKind::Cross` (two-stage gcc) and implies
`--init-target`. Note the two roles of the tuple: `crossdev -t <tuple>` drives
the *setup action*; `--cross <tuple>` (global) targets the sysroot for later
*use* (`stages`, plain `em <atom>`). Both carry the same tuple in practice.

### Lifecycle × topology map

| command | topology after | BROOT |
|---|---|---|
| `em setup --prefix P` | `Overlayed` | `/` (host) |
| `em setup --local` | `Single { ~/.gentoo }` | `/` → `~/.gentoo` (after toolchain) |
| `em --root R toolchain --setup` | `Dual { broot: /, target: R }` | `/` |
| `em --local toolchain --setup` | `Single { ~/.gentoo }` | `~/.gentoo` |
| `em crossdev -t T --init-target` | `Dual { broot: EROOT, target: EROOT/usr/T }` | EROOT |
| `em --local --cross T ...` | `Dual { broot: ~/.gentoo, target: ~/.gentoo/usr/T }` | `~/.gentoo` |

The BROOT column shows axis 2 in action: `--local`'s BROOT *moves* from `/`
(host seed) to `~/.gentoo` (self-hosting) over its lifecycle, without the
topology variant changing — confirming BROOT identity is a runtime property,
not a structural one.



The code calls both "stage1" but they compose
([`crossdev/stages.rs`](../portage-cli/src/crossdev/stages.rs)):

1. **Toolchain stage1** (`toolchain_plan`, `BootstrapKind::Cross`/`Native`) —
   the chicken-and-egg bootstrap of the *compiler itself*: binutils → headers
   → libc-headers (`--nodeps`) → gcc-stage1 → libc → gcc-stage2. Cross needs
   the two-stage split; native (`CHOST==CBUILD`) builds full glibc+gcc in one
   pass because the seed compiler already targets that arch.
2. **`packages.build` stage1** (`stage1_plan`, catalyst's `stage1/chroot.sh`) —
   *assumes* a toolchain already exists in the root, then emerges the minimal
   bootable package *set* (baselayout with `USE=build` `--nodeps`, then
   `packages.build` with `USE="-* build"`).

`stage3` = full `@system` into the root. `stage4` = stage3 + a custom/`@world`
set — "the bootable/installable system."

## Satisfaction-root mapping (current code, not yet the variant)

Today the routing is encoded in *two vocabularies* that must agree:

- **Solver side** (`portage-atom-pubgrub/src/provider/solve.rs`):
  `cross_target_runtime_deps` / `host_native_deps` / `broot_filtered` stamp
  `MergeRoot::Target` / `MergeRoot::Host` per dep class. `host_aliases`
  (`provider/mod.rs:708`) maps a `Host`-flavored package to its `Target` data;
  `package_data()` is the alias-resolving accessor (a raw `packages.get()` is
  the bug behind `208c818`).
- **Post-solve side** (`portage-cli/src/preflight.rs`, `bdepend_avail.rs`):
  `Avail::initial_bdepend(host_roots)` / `initial_depend(roots)`, and
  `preflight::check`'s `match planned.merge_root` arms.

The two sides can't share code directly (one speaks `PortagePackage`/
`VersionData`, the other `Cpv`/VDB) — that's a real boundary, not gratuitous
duplication. The invariant they must both honour is the table above. The
variant refactor's payoff is that both sides ask
`topology.satisfaction_root(class)` instead of re-deriving it from positional
`&Roots` arguments, retiring the `host_roots`-threading smell
(`c421c95`/`732aefe`/`0e9b3e0` were all "wrong root at one site" bugs).

## Status

- **Done** — `Roots` struct, three-root split, builder env threading
  (`run_phase` sets `PORTAGE_CONFIGROOT`/`ROOT`/`SYSROOT`/`BROOT`), per-class
  VDB checks, `MergeRoot` on solver nodes, Host-BDEPEND scheduling,
  `BUILD_PKG_CONFIG_LIBDIR` for cross.
- **TODO (this doc's proposal)** — replace the flat `Roots` bag with
  `RootTopology` enum + `cross: CrossArch`; derive `satisfaction_root(class)`
  from the variant; privatize `provider.packages` behind `package_data()` so
  the `host_aliases` invariant can't be violated by a stray `get()`.
- **Deferred (out of scope here)** — Tier 3 mutable-BROOT bootstrap on a
  foreign host (`build-environment.md`), zero-config merged sysroot via
  `fuse-overlayfs` (M3).
