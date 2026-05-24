# portage-repo Status

## PMS 9 Compliance

Target specification: [PMS 9](https://projects.gentoo.org/pms/9/pms.html)

### What works

#### Repository layout (PMS 4)
- `Repository::open()` — reads `layout.conf` and `profiles/repo_name`
- Category / package / ebuild enumeration via `categories()`, `packages()`, `ebuilds()`
- Metadata cache reading via `cache_entry(&cpv)` (`metadata/md5-cache/`)
- Profile descriptions (`profiles.desc`), USE flag descriptions, arch list, mirrors
- Eclass and license listing
- Dotfiles skipped in category/package enumeration
- `profiles/eapi` — default EAPI for profile dirs (`Repository::profiles_eapi`)
- `profiles/package.mask` — repo-level package masks (`Repository::repo_package_mask`)
- `profiles/desc/` — USE_EXPAND flag descriptions (`Repository::use_expand_names`,
  `Repository::use_expand_desc`)
- `profiles/use.desc` / `profiles/use.local.desc` — global and per-package USE flag
  descriptions (`Repository::use_desc`, `Repository::use_local_desc`)
- `UseExpand` — buckets a flat USE flag list into groups by prefix, with `Repository::use_expand()`
  convenience constructor and `UseExpand::from_var()` for the shell's `$USE_EXPAND`
- `Package::metadata_xml()` — parses `metadata.xml` USE flag descriptions (`PkgMetadata`)

#### Profiles (PMS 5)
- `parent`, `eapi`, `packages`, `package.mask`, `package.use`
- `use.force`, `use.mask`, `use.stable.force`, `use.stable.mask`
- `package.use.force`, `package.use.mask`, `package.use.stable.force`,
  `package.use.stable.mask`
- `make.defaults` sourced through embedded bash shell
- Profile inheritance / stacking (`ProfileStack`): depth-first parent
  traversal with cycle/diamond-dedup, incremental `-` removal for `use.*`
  and `package.mask`, directory-as-file support (PMS 5.1, 5.2.5)
- `deprecated` file check (`ProfileStack::is_deprecated`)
- `Repository::profile_stack()` convenience constructor

#### Master repository eclass resolution (PMS 4.7, 10.1)
- `Repository::open_with_masters()` — opens a repo and recursively resolves
  master repositories from a base directory (depth-first, with cycle detection)
- `Repository::shell_with_masters()` — creates an `EbuildShell` with master
  eclass directories prepended, so `inherit` finds eclasses from masters

#### Embedded bash shell (PMS 10, 12)
- Full embedded shell via brush-core with the winnow parser
- Eclass sourcing via `inherit()` with `INHERITED` tracking and `ECLASS` scoping
- PMS 10.2 eclass metadata key accumulation: `IUSE`, `REQUIRED_USE`, `DEPEND`,
  `BDEPEND`, `RDEPEND`, `PDEPEND`, `IDEPEND` (all EAPIs), plus `PROPERTIES` and
  `RESTRICT` (EAPI 8+) are saved/cleared/restored around each eclass `source`
- `EXPORT_FUNCTIONS` phase alias creation
- PM-provided variables: `CATEGORY`, `PN`, `PV`, `PVR`, `P`, `PF`, `PR`, `FILESDIR`,
  `EBUILD`, `WORKDIR`, `S`, `T`, `TMPDIR`, `HOME`, `D`, `DISTDIR`,
  `EBUILD_PHASE`, `EBUILD_PHASE_FUNC`, `ROOT`, `MERGE_TYPE`,
  `EPREFIX`/`ED`/`EROOT` (EAPI 3+), `SYSROOT`/`ESYSROOT`/`BROOT` (EAPI 7+)

#### Metadata extraction (PMS 7, 14)
- All 18 PMS metadata variables extracted after sourcing:
  `EAPI`, `DESCRIPTION`, `SLOT`, `HOMEPAGE`, `SRC_URI`, `LICENSE`, `KEYWORDS`,
  `IUSE`, `REQUIRED_USE`, `RESTRICT`, `PROPERTIES`, `DEPEND`, `RDEPEND`,
  `BDEPEND`, `PDEPEND`, `IDEPEND`, `INHERITED`, `DEFINED_PHASES`
- `EAPI` detected by regex before sourcing per PMS 7.3.1 and set in the shell
  environment so it is available during sourcing
- `DEFINED_PHASES` computed from shell function table after sourcing (PMS 7.4)
- Comparison tooling: `examples/regen_cache.rs` sources every ebuild and diffs
  against the md5-cache

#### Portage-specific shell functions (PMS 12)
- `die`, `nonfatal`
- `has`, `hasv`, `hasq`
- `use`, `usev`, `usex`, `use_enable`, `use_with`, `in_iuse` (stubs — always return false)
- `ver_cut`, `ver_rs`, `ver_test` (match Gentoo reference implementation)
- `has_version`, `best_version` (stubs)
- Debug/output no-ops: `einfo`, `ewarn`, `eerror`, `debug-print`, etc.
- Build/install stubs: `econf`, `emake`, `eapply`, `dobin`, `doins`, etc.

---

### Known limitations

#### USE flag stubs always return false (by design)
`use()`, `usev()`, `usex()` always return 1 (false). Correct for metadata
extraction (no profile is active), but ebuilds that conditionally set metadata
variables based on USE flags at source time will produce different values than
a real `pmaint regen` with an active profile.

#### Phase-specific PM variables are approximate (by design)
`EBUILD_PHASE` is always `depend`, `MERGE_TYPE` is always `source`. Correct
for metadata extraction; would need richer context for phase execution.

#### `BASH_COMPAT` per EAPI (PMS 6, Table 6.1)
`BASH_COMPAT` is not set per EAPI. PMS requires bash 3.2 for EAPIs 0–5,
4.2 for EAPIs 6–7, 5.0 for EAPI 8, 5.3 for EAPI 9. Brush does not expose
this setting in a meaningful way; no mismatches observed in practice.

#### Legacy metadata cache format (PMS 14.2)
Only md5-dict (`metadata/md5-cache/`) is supported. The positional line-based
`metadata/cache/` format is not implemented. No modern repository uses it.

## Running the full comparison

```bash
# Single ebuild
cargo run --release --example regen_cache -- gentoo 'dev-lang/rust-1.88.0'

# Whole category
cargo run --release --example regen_cache -- gentoo 'dev-lang/*'

# Full tree (~32K ebuilds, slow)
cargo run --release --example regen_cache -- gentoo
```

Output goes to stderr (progress + errors + diffs) and stdout (final stats).
Exit code is 1 if there are any errors or mismatches.
