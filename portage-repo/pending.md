# Pending work: ebuild phase execution

Two-track plan: Rust builtins for real phase execution, plus brush compat
test cases that document current shell gaps.

---

## Track 1 — Brush compatibility test cases

Five YAML test files in `brush/brush-shell/tests/cases/compat/`,
committed on branch `for-portage-repo` in the brush repo.

- [x] `portage_gap1_bare_function_body.yaml` — bare `[[ ]]`/`(( ))` as
  function body; 2 of 3 cases fail (gap still open in brush parser)
- [x] `portage_gap2_extglob_in_brackets.yaml` — all pass (already fixed)
- [x] `portage_gap3_bash_rematch.yaml` — all pass (already fixed)
- [x] `portage_gap4_printf_v.yaml` — all pass (already fixed)
- [x] `portage_gap5_mapfile.yaml` — all pass (already fixed)

Gap 1 (bare compound-command function body) is the only remaining brush
parser limitation that affects portage code.  The 74 `___eapi_*` predicates
use that syntax; they are worked around by the `EapiPredicateCommand` Rust
builtin rather than being fixed in brush.

---

## Track 2 — Rust builtins for phase execution

`builtins.rs` registers bash no-op stubs so eclasses don't error during
metadata extraction (they call `econf`, `einfo`, etc. at source time).
`init_build_env()` unsets those stubs before any build phase so the Rust
builtins below take effect.

### P0 — Phase dispatch

- [x] `___eapi_*` — 74 EAPI predicate builtins, dispatched via
  `context.command_name` against a static `match`
- [x] `__ebuild_phase_funcs` — wires up `default()` / `default_<phase>()`
  and installs a fallback `<phase>()` when the ebuild didn't define it
- [x] `__eapi0_pkg_nofetch`, `__eapi0_src_unpack`, `__eapi0_src_compile`,
  `__eapi0_src_test`, `__eapi1_src_compile`, `__eapi2_src_prepare`,
  `__eapi2_src_configure`, `__eapi2_src_compile`, `__eapi4_src_install`,
  `__eapi6_src_prepare`, `__eapi6_src_install`, `__eapi8_src_prepare` —
  bash implementations in `PHASE_DEFAULT_FUNCTIONS`, called by
  `__ebuild_phase_funcs`

Known gaps in `__eapi0_src_test`:
- [x] missing `-j1` for EAPI ≤ 4 — now uses `___eapi_default_src_test_disables_parallel_jobs`
- [x] MAKEFLAGS jobserver guard — `strip_jobserver_tokens()` called in `init_build_env` strips `--jobserver-auth`/`--jobserver-fds` before any phase runs

Known gap in `EbuildPhaseFuncsCommand`:
- [x] does not install `default_<other_phase>()` error stubs — now installed in commit 192bd44

### P1 — Output helpers

- [x] `einfo`, `elog`, `ewarn`, `eerror`, `eqawarn`, `einfon`
- [x] `ebegin`
- [x] `eend`

### P2 — Build helpers

- [x] `emake` — spawns `${MAKE:-make}` with `$MAKEOPTS $EXTRA_EMAKE`
- [x] `econf` — spawns `./configure` with EAPI-appropriate flags;
  probes `--help` for conditional flags with word-boundary guard
- [x] `assert` — bash function; captures `PIPESTATUS` before any other
  command to avoid clobbering
- [x] `nonfatal` — sets `PORTAGE_NONFATAL=1`, runs `"$@"`, unsets on return
- [x] `eapply` — bash function; `patch -p1 < file` loop
- [x] `eapply_user` — stub (`:`)
- [x] `einstalldocs` — real impl: respects `$DOCS` array/string, auto-installs README*/CHANGES*/AUTHORS*/NEWS*, handles `$HTML_DOCS`
- [x] `get_libdir` — checks `LIBDIR_${ABI}`, defaults to `lib`
- [x] `edo` — einfo + exec, die on failure (EAPI 9)

Known gaps shared by `emake` and `econf`:
- [ ] `MAKEOPTS` / `EXTRA_EMAKE` / `EXTRA_ECONF` are split on whitespace;
  quoted values with internal spaces (portage uses `eval` for `EXTRA_ECONF`,
  bug #457136) are not handled

### P3 — Install helpers

Implemented as bash functions in `INSTALL_HELPERS` const (shell.rs), loaded by
`init_build_env()`.  Stubs in `builtins.rs` remain for metadata-only mode.

- [x] `into` / `insinto` / `exeinto` / `docinto` — state setters
- [x] `insopts` / `exeopts`
- [x] `dobin` / `newbin`
- [x] `dosbin` / `newsbin`
- [x] `doins` / `newins` (with `-r` for recursive copy)
- [x] `doexe` / `newexe`
- [x] `dolib.a` / `dolib.so`
- [x] `dodir` / `keepdir`
- [x] `dodoc` / `newdoc` (with `-r`)
- [x] `doman` / `newman`
- [x] `dosym` (EAPI 8 `-r` via `python3 os.path.relpath`)
- [x] `doheader` / `newheader` (with `-r`)
- [x] `docompress` / `dostrip` — record include/exclude lists
- [x] `doinitd` / `doconfd` / `fperms` / `fowners`
- [x] `__eapi4_src_install` DOCS: calls `dodoc "${DOCS[@]}"` / `dodoc ${DOCS}`

- [x] `dolib` (bare) — routes to `dolib.so` for `.so`/`.so.*`, `dolib.a` otherwise
- [x] `newlib.a` / `newlib.so`

### P4 — Unpack

- [x] `unpack` — Rust builtin dispatching by extension: `.tar.{gz,bz2,xz,zst,lz,lzma}`,
  `.tgz`/`.tbz2`/`.tbz`/`.txz`, `.zip`, `.gz`/`.bz2`/`.xz`/`.lzma`/`.zst` (piped),
  `.7z`/`.rar`/`.lha` (EAPI ≤ 7 only); bare names resolved via `$DISTDIR`;
  absolute paths checked against EAPI ≥ 6; case-insensitive for EAPI ≤ 5
- [x] `$A` computed from `$SRC_URI` via `SrcUriEntry::parse` with USE-conditional
  evaluation; set before any phase function runs
- [x] phase working directory: `src_unpack` and `pkg_nofetch` cd to `$WORKDIR`;
  all other phases cd to `$S` (with `$WORKDIR` fallback)
- [x] build-phase environment variables exported via `export` so external tools
  (`make`, `./configure`, portage ebuild-helpers) inherit them

### P5 — Package query stubs

Already wired as bash stubs; no Rust builtin needed until dep-solving is in scope.

- [x] `has_version` — returns 1
- [x] `best_version` — returns empty string + 1

---

## End-to-end test

To exercise the full configure → compile → install flow:

```bash
# Download and unpack source manually (unpack not yet implemented)
mkdir -p /tmp/hello-build/work && cd /tmp/hello-build/work
wget https://ftp.gnu.org/gnu/hello/hello-2.12.2.tar.gz
tar xf hello-2.12.2.tar.gz

# Run phases
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 configure --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 compile    --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 install    --work-dir /tmp/hello-build
```

Full sequence (using hello-2.12.3 — 2.12.2 has a gnulib/glibc incompatibility on newer systems):

```bash
# Download distfile once
wget https://ftp.gnu.org/gnu/hello/hello-2.12.3.tar.gz -O ~/.cache/distfiles/hello-2.12.3.tar.gz

WORK=/tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.3 unpack    --work-dir $WORK
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.3 configure --work-dir $WORK
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.3 compile   --work-dir $WORK
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.3 install   --work-dir $WORK
# Binary installed to $WORK/image/usr/bin/hello
```
