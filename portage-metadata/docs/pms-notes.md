# PMS Notes for portage-metadata

Research notes on the [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/latest/pms.html) chapters relevant to this crate.

## EAPI (Chapter 6)

The EAPI controls which features and behaviors are available. Key additions by EAPI:

| EAPI | Notable Additions |
|------|-------------------|
| 0 | Base |
| 1 | SLOT deps, IUSE defaults (+/-) |
| 2 | SRC_URI `->` renaming, USE deps, `src_prepare`/`src_configure` |
| 3 | PROPERTIES, prefix support |
| 4 | REQUIRED_USE, `pkg_pretend`, `^^` operator |
| 5 | Sub-slots, slot operators (`:=`, `:*`), `??` operator |
| 6 | `eapply`/`eapply_user`, bash 4.2 minimum |
| 7 | BDEPEND, SYSROOT/BROOT |
| 8 | IDEPEND, USE-conditional PROPERTIES/RESTRICT |

## Ebuild-Defined Variables (Chapter 7)

### Mandatory Variables (PMS 7.2)

| Variable | Description |
|----------|-------------|
| EAPI | Ebuild API version |
| DESCRIPTION | One-line package description |
| SLOT | Slot for parallel installation |
| HOMEPAGE | Upstream URL(s) |
| SRC_URI | Source download URIs |
| LICENSE | License expression |
| KEYWORDS | Architecture keywords |
| IUSE | USE flags the ebuild understands |

### Dependency Variables

| Variable | Description | Since EAPI |
|----------|-------------|-----------|
| DEPEND | Build-time dependencies | 0 |
| RDEPEND | Runtime dependencies | 0 |
| PDEPEND | Post-merge dependencies | 0 |
| BDEPEND | Build-host dependencies (cross-compile) | 7 |
| IDEPEND | Install-time dependencies | 8 |

### Other Variables

| Variable | Description | Since EAPI |
|----------|-------------|-----------|
| REQUIRED_USE | USE flag constraints | 4 |
| RESTRICT | Restrictions (mirror, test, etc.) | 0 |
| PROPERTIES | Package properties (live, interactive) | 3 |
| DEFINED_PHASES | Phase functions the ebuild defines | 0 |
| INHERITED | Eclasses inherited by the ebuild | 0 |

## Phase Functions (Chapter 9)

| Phase | Purpose | Since EAPI |
|-------|---------|-----------|
| pkg_pretend | Pre-flight checks | 4 |
| pkg_setup | Environment setup | 0 |
| src_unpack | Extract sources | 0 |
| src_prepare | Apply patches | 2 |
| src_configure | Run configure | 2 |
| src_compile | Build | 0 |
| src_test | Run tests | 0 |
| src_install | Install to image | 0 |
| pkg_preinst | Before merge | 0 |
| pkg_postinst | After merge | 0 |
| pkg_prerm | Before unmerge | 0 |
| pkg_postrm | After unmerge | 0 |
| pkg_config | Post-install config | 0 |
| pkg_info | Display info | 0 |
| pkg_nofetch | Handle restricted fetch | 0 |

In the metadata cache, DEFINED_PHASES uses short names without prefix:
`compile configure install` (not `src_compile src_configure src_install`).
The special value `-` means no phases are defined.

## Metadata Cache Format (Chapter 14)

### md5-dict format (PMS 14.3)

Located at `metadata/md5-cache/<category>/<package>-<version>`.

Each file contains `KEY=VALUE` lines in arbitrary order. Empty values may be
omitted entirely.

### Standard keys

All the ebuild-defined variables listed above, plus:

| Key | Description |
|-----|-------------|
| `_md5_` | MD5 checksum of the ebuild file (RFC 1321 hex) |
| `_eclasses_` | Tab-separated pairs: `name\tchecksum\tname\tchecksum\t...` |

### Example

```
DEFINED_PHASES=install test unpack
DEPEND=>=sys-devel/clang-10.0.0_rc1:*
DESCRIPTION=Python bindings for sys-devel/clang
EAPI=7
HOMEPAGE=https://llvm.org/
IUSE=test python_targets_python3_6 python_targets_python3_7
KEYWORDS=~amd64 ~x86
LICENSE=Apache-2.0-with-LLVM-exceptions UoI-NCSA
RDEPEND=>=sys-devel/clang-10.0.0_rc1:*
REQUIRED_USE=|| ( python_targets_python3_6 python_targets_python3_7 )
RESTRICT=!test? ( test )
SLOT=0
SRC_URI=https://github.com/llvm/llvm-project/archive/llvmorg-10.0.0-rc1.tar.gz
_eclasses_=llvm.org	4e92abc	multibuild	40fe1234
_md5_=4539d849d3cea8ac84debad9b3154143
```

## Expression Grammar

Several metadata variables use a dependency-specification-like grammar (PMS 8.2):

- **Atoms**: `category/package` or `>=category/package-version:slot[use]`
- **Any-of groups**: `|| ( a b c )`
- **USE-conditional groups**: `flag? ( ... )` and `!flag? ( ... )`
- **Exactly-one-of**: `^^ ( a b c )` (REQUIRED_USE, EAPI 4+)
- **At-most-one-of**: `?? ( a b c )` (REQUIRED_USE, EAPI 5+)

Used by: DEPEND, RDEPEND, BDEPEND, PDEPEND, IDEPEND, LICENSE, SRC_URI,
REQUIRED_USE, and (in EAPI 8) RESTRICT and PROPERTIES.

## Repository Layout (Future: portage-repo)

```
repository/
  metadata/
    layout.conf          # masters, cache-formats, profile-formats
    md5-cache/           # <-- this crate reads/writes these
  profiles/
    repo_name
    categories
    eapi
  eclass/
  <category>/
    <package>/
      <package>-<version>.ebuild
      Manifest
      metadata.xml
      files/
  licenses/
```

## brush-parser Evaluation (Future: portage-ebuild)

[brush](https://github.com/reubeno/brush) is a Rust bash shell that could
potentially source ebuilds to extract metadata. Key considerations:

- MSRV concern: brush requires nightly or very recent stable
- Eclass resolution: would need to set up the full inherit chain
- Correctness: would need to faithfully reproduce Portage's bash environment

For now, portage-metadata reads the pre-computed cache, avoiding the need for
a bash interpreter entirely.
