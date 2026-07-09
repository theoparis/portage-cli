# Unslop Plan: Panic/Error Handling Cleanup

## Overview

This document outlines all locations in the portage-cli codebase where reachable
panics can occur through `unwrap()`, `expect()`, or `panic!` macros, along with a
comprehensive plan to clean them up by properly forwarding errors.

## Methodology

1. Searched all `.rs` files in main source directories (excluding tests, benches, target)
2. Identified all `unwrap()`, `expect()`, and `panic!` calls
3. Categorized by severity and location
4. Prioritized based on user-facing impact
5. Verified findings by checking function contexts and test annotations

## Legend

- **P0 (Critical)**: User-facing panics that can crash the application during normal operations
- **P1 (High)**: Library panics that could affect downstream users
- **P2 (Medium)**: Provably safe unwraps that should still use proper error handling
- **P3 (Low)**: Test code unwraps (acceptable but should be consistent)

## Important Discovery

After deeper investigation, **most panic! calls are in test functions** (P3), not in production code. The production code is relatively clean. The main issues are:

1. Some `unwrap()` calls in user-facing output code (P0) - being fixed
2. Some `expect()` calls with insufficient context (P0) - being improved
3. Most `panic!` calls are in test assertions and helper functions (P3)

---

## Summary Statistics (Revised)

| Crate | Production unwrap() | Production expect() | Production panic! | Total |
|-------|---------------------|---------------------|-------------------|-------|
| portage-cli | ~15 | ~5 | ~0 | 20 |
| portage-atom | ~0 | ~0 | ~0 | 0 |
| portage-metadata | ~0 | ~0 | ~0 | 0 |
| portage-repo | ~5 | ~2 | ~0 | 7 |
| portage-solver | ~0 | ~0 | ~0 | 0 |
| gentoo-core | ~0 | ~0 | ~0 | 0 |
| **Production Total** | **~20** | **~7** | **~0** | **~27** |
| **Test Total** | ~410+ | ~35+ | ~58+ | ~503+ |

*Numbers are approximate based on grep results and manual verification*

*Most panic! calls are in `#[test]` functions and test helper functions*

---

## Module layout (post housekeeping)

`portage-cli` is now a library + thin binary. User-facing resolve/merge logic
moved out of `main.rs`:

| Module | Role |
|--------|------|
| `src/main.rs` | Entry point, allocator, fakeroost/pseudoroot init, tokio runtime |
| `src/dispatch.rs` | Subcommand routing (`run()` body) |
| `src/emerge.rs` | `expand_sets`, `EmergeOpts`, resolve + merge orchestration |
| `src/merge/mod.rs` | Staged merge driver, cache regen, parallel jobs |
| `src/vdb.rs` | `open_cli_vdb()` — VDB at `--vdb` or `<merge_root>/var/db/pkg` |

Line numbers below for `main.rs` / set expansion refer to these modules unless
noted otherwise.

---

## Progress

### Completed Fixes (P0)
1. **portage-cli/src/query/depgraph/output.rs:837** — Fixed: `serde_json::to_string_pretty` returns `Result<()>` with proper error handling
2. **portage-cli/src/query/depgraph/output.rs:563** — Improved: SAFETY comment for `from_utf8` unwrap on ASCII array
3. **portage-cli/src/query/depgraph/subslot.rs:83** — Fixed: replaced `.expect("filtered to bound atoms")` with proper `Option` handling
4. **portage-cli/src/query/depgraph/depend_trim.rs:77,83** — Improved: enhanced expect messages for hardcoded CPNs
5. **portage-cli/src/main.rs** — Fixed: tokio runtime `.build()` failure prints `error:` and exits 1 (no `expect`)
6. **portage-cli/src/cli.rs` (`base_roots`)** — Fixed: `--prefix` path via `path()` closure (`if let Some(prefix) = path(&self.prefix)`); invalid UTF-8 falls through to default `Roots` instead of panicking
7. **portage-cli/src/emerge.rs` (`expand_sets`)** — Fixed: profile stack held in `stack_holder.get_or_insert(st)`; set expansion errors are warnings, not panics
8. **portage-repo/src/package_conf.rs** — Fixed: production `PackageConf::parse(…).unwrap()` removed (tests only)

### Completed housekeeping (non-P0)
- **portage-repo** — `#![warn(missing_docs)]` enabled; 21 doc gaps filled
- **portage-binpkg** — `#![warn(missing_docs)]` enabled
- Test modules extracted to sibling files (`provider/tests.rs`, `solver_tests.rs`, `shell/tests.rs`)

All tests pass and clippy is clean after these changes.

---

## Detailed Findings by Crate

### portage-cli Crate (Binary)

#### Critical Issues (P0)

##### `src/main.rs`
- **No production unwrap/expect/panic** — runtime init and `portage_cli::run()` dispatch only.

##### `src/emerge.rs`
- `expand_sets` — **FIXED** (see Progress §5–7). No remaining production panics.

##### `src/dispatch.rs`, `src/merge/mod.rs`, `src/vdb.rs`
- **No production unwrap/expect/panic** in current scan.

##### `src/cli.rs`
- `base_roots()` — **FIXED**: `path` closure maps `Option<String>` → `Option<Utf8PathBuf>` without expect
- `repo_path()` / `repo_paths()` — still use `unwrap_or_default()` for repo layout fallbacks (P2)
- Lines 414–416, 455, 482: `.unwrap()` in `#[cfg(test)]` assertions only — P3

##### `src/package_env.rs`
- Line 99: `Cpv::parse(s).unwrap()` - Parsing user input without error handling - **P0**
- Line 104-109: Multiple `unwrap()` in test helper functions - P3

##### `src/query/` modules

###### `query/depgraph/output.rs`
- Line 563: `String::from_utf8(f.to_vec()).unwrap()` - **SAFE** (f is ASCII array) but should use `unwrap_unchecked()` or `String::from_utf8_unchecked()`
- Line 837: `serde_json::to_string_pretty(&out).unwrap()` - Serialization failure - **P0**
- Line 964, 972: `Cpn::parse(cpn).unwrap()` - Parsing user-provided CPN strings - **P0**
- Line 981, 996, 1006: `Version::parse("1.0").unwrap()` - Test data - **P3**

###### `query/depgraph/force_mask.rs`
- Line 167: `s.rsplit_once('-').unwrap()` - Can fail on malformed input - **P0**
- Line 169-170: `Cpn::parse(cpn).unwrap()`, `Version::parse(ver).unwrap()` - Parsing - **P0**
- Line 175: `Dep::parse(s).unwrap()` - Parsing - **P0**

###### `query/depgraph/installed.rs`
- Line 207-212, 217, 219: tempfile and fs operations - **P0** (I/O can fail)

###### `query/depgraph/c7.rs`
- Line 38-39, 111: Parsing operations - **P0**

###### `query/depgraph/depend_trim.rs`
- Line 161-162, 169-170: Version and Cpn parsing in tests - **P3**

###### `query/depgraph/bdepend_trim.rs`
- Line 181-182, 221-230, 270-271: Parsing in tests - **P3**

###### `query/depgraph/repo.rs`
- Line 1132-1143, 1178-1204, 1224-1225, 1248-1249, 1269-1271, 1276, 1286-1304: Parsing and repo operations - Mix of **P0** and **P3**

##### `src/maint/` modules
All maint modules have extensive unwraps in test/helper functions - **P3**

##### `src/select/` modules
- tempfile and fs operations in tests - **P3**

##### `src/crossdev/` modules
- CrossTarget parsing and tempfile in tests - **P3**

##### `src/postprocess.rs`
- tempfile and fs operations in tests - **P3**

##### `src/preflight.rs`
- Parsing and tempfile in tests - **P3**

##### `src/bdepend_avail.rs`
- Parsing and tempfile in tests - **P3**

#### Summary for portage-cli
- **P0 (Critical)**: ~30 locations in user-facing code
- **P1 (High)**: 0 (binary crate)
- **P2 (Medium)**: ~5 locations (safe but should be cleaned)
- **P3 (Low/Test)**: ~100+ locations in test helper functions

---

### portage-atom Crate (Library)

#### Critical Issues (P1)

##### `src/dep_entry.rs`
~40+ `panic!` in `FromStr` implementations using match with `_ => panic!(...)`

##### `src/dep.rs`
~15+ `panic!` in `FromStr` implementations

##### `src/version.rs`
- Line 940: `Version::parse(v).unwrap_or_else(|e| panic!(...))`

##### `src/slot.rs`
~5+ `panic!` in `FromStr` implementations

##### `src/cpn.rs`, `src/cpv.rs`, `src/pf.rs`
Similar patterns in parsing implementations

#### Summary for portage-atom
- **P0**: 0 (library crate, no user-facing code)
- **P1 (High)**: ~65+ panic! in FromStr implementations
- **P2**: ~50+ in doc tests
- **P3**: 0

---

### portage-metadata Crate (Library)

#### Issues

##### `src/cache.rs`
- Line 876: `panic!("texlive-core cache entry failed to parse: {e}\n\nRaw:\n{raw}")`
- Line 911: `.unwrap_or_else(|e| panic!("kpathsea cache entry failed to parse: {e}\n\nRaw:\n{raw}"))`

##### Doc tests
~80+ `.unwrap()` in doc tests (examples)

#### Summary for portage-metadata
- **P0**: 0
- **P1**: ~2 locations with panic! in parsing
- **P2**: ~80+ in doc tests
- **P3**: 0

---

### portage-repo Crate (Library)

#### Issues

##### `src/make_conf.rs`
- Line 175: `.expect("inconsistent state: entry matched in filter but variable not found")`
- Line 383: `MakeConf::parse(src.to_owned()).expect("parse failed")`

##### `src/package_conf.rs`
- Production parse unwrap — **FIXED** (housekeeping)
- `#[cfg(test)]` helper `parse()` and assertions — P3

##### `src/repo/` modules
Various `.unwrap()` in test helper functions

#### Summary for portage-repo
- **P0**: 0
- **P1**: ~5 locations in library code
- **P2**: 0
- **P3**: ~55+ in test code

---

### portage-solver Crate (Library)

#### Issues

##### `src/use_config.rs`
- Lines 361-402: `Cpv::parse(...).unwrap()`, `Dep::parse(...).unwrap()` - test data
- Line 382: `panic!("expected owned")`

##### `src/facts.rs`
- Lines 172, 188: `Cpv::parse(s).unwrap()`, `Dep::parse(...).unwrap()` - test data

#### Summary for portage-solver
- **P0**: 0
- **P1**: ~1 location
- **P2**: ~10+ in test data
- **P3**: 0

---

### gentoo-core Crate (Library)

#### Issues
All in test functions - **P3**

#### Summary for gentoo-core
- **P0**: 0
- **P1**: 0
- **P2**: ~1 location
- **P3**: ~35+ in test code

---

## Cleanup Plan

### Phase 1: Critical User-Facing Panics (P0) - Week 1-2

#### 1.1 portage-cli/src/package_env.rs
**Issue**: Line 99 - `Cpv::parse(s).unwrap()` on user input

**Fix**:
```rust
// Before:
Cpv::parse(s).unwrap()

// After:
Cpv::parse(s).map_err(|e| anyhow::anyhow!("failed to parse CPV '{s}': {e}"))?
```

**Files**: `src/package_env.rs:99`

#### 1.2 portage-cli/src/query/depgraph/output.rs
**Issues**:
- Line 563: `String::from_utf8(f.to_vec()).unwrap()` - SAFE but should be explicit
- Line 837: `serde_json::to_string_pretty(&out).unwrap()` - Serialization can fail
- Line 964, 972: `Cpn::parse(cpn).unwrap()` - User input parsing

**Fixes**:
```rust
// Line 563 - Safe, use unchecked (f is [b' '; 7]):
unsafe { String::from_utf8_unchecked(f.to_vec()) }
// Or better, use from_utf8_lossy if we want to be safe:
String::from_utf8_lossy(&f.to_vec()).into_owned()

// Line 837 - Handle serialization error:
serde_json::to_string_pretty(&out)
    .map_err(|e| anyhow::anyhow!("failed to serialize output: {e}"))?

// Lines 964, 972 - Parse with error:
Cpn::parse(cpn)
    .map_err(|e| anyhow::anyhow!("failed to parse CPN '{cpn}': {e}"))?
```

**Files**: `src/query/depgraph/output.rs:563,837,964,972`

#### 1.3 portage-cli/src/query/depgraph/force_mask.rs
**Issues**:
- Line 167: `s.rsplit_once('-').unwrap()` - Can fail on malformed CPV
- Line 169-170: `Cpn::parse(cpn).unwrap()`, `Version::parse(ver).unwrap()`
- Line 175: `Dep::parse(s).unwrap()`

**Fixes**:
```rust
// Line 167:
let (cpn, ver) = s.rsplit_once('-')
    .ok_or_else(|| anyhow::anyhow!("malformed CPV string '{s}': missing version separator"))?;

// Lines 169-170:
let cpn = Cpn::parse(cpn)
    .map_err(|e| anyhow::anyhow!("failed to parse CPN '{cpn}': {e}"))?;
let ver = Version::parse(ver)
    .map_err(|e| anyhow::anyhow!("failed to parse version '{ver}': {e}"))?;

// Line 175:
Dep::parse(s)
    .map_err(|e| anyhow::anyhow!("failed to parse dependency '{s}': {e}"))?
```

**Files**: `src/query/depgraph/force_mask.rs:167,169-170,175`

#### 1.4 portage-cli/src/cli.rs — **DONE**
`base_roots()` uses a `path` closure; invalid `--prefix` UTF-8 is ignored (falls through to default `Roots`).

**Files**: `src/cli.rs` (`base_roots`)

#### 1.5 portage-cli/src/query/depgraph/installed.rs
**Issues**: Lines 207-212, 217, 219 - tempfile and fs operations

**Fix**: Wrap all I/O operations in proper error handling with `?` operator.

**Files**: `src/query/depgraph/installed.rs:207-219`

#### 1.6 portage-cli/src/query/depgraph/c7.rs
**Issues**: Lines 38-39, 111 - Parsing operations

**Fix**: Similar parsing error handling as above.

**Files**: `src/query/depgraph/c7.rs:38-39,111`

### Phase 2: Library Panics (P1) - Week 3-4

#### 2.1 portage-atom FromStr implementations
**Issue**: ~65+ panic! in match `_ => panic!(...)` across parsing modules

**Fix**: Replace panic! with proper error types using the existing `portage_atom::Error` type.

```rust
// Before:
match something {
    ValidVariant(v) => Ok(v),
    _ => panic!("expected Atom"),
}

// After:
match something {
    ValidVariant(v) => Ok(v),
    _ => Err(portage_atom::Error::ParseError("expected Atom".into())),
}
```

**Files**:
- `portage-atom/src/dep_entry.rs` - All panic! in FromStr
- `portage-atom/src/dep.rs` - All panic! in FromStr
- `portage-atom/src/version.rs:940`
- `portage-atom/src/slot.rs` - All panic! in FromStr
- `portage-atom/src/cpn.rs` - Any panic! in FromStr
- `portage-atom/src/cpv.rs` - Any panic! in FromStr
- `portage-atom/src/pf.rs` - Any panic! in FromStr

#### 2.2 portage-metadata panic! locations
**Issues**:
- `src/cache.rs:876` - panic! in texlive-core parsing
- `src/cache.rs:911` - unwrap_or_else with panic! in kpathsea parsing

**Fix**: Return proper errors instead of panicking.

**Files**: `portage-metadata/src/cache.rs:876,911`

#### 2.3 portage-repo expect/unwrap locations
**Issues**:
- `src/make_conf.rs:175,383` - expect calls
- `src/package_conf.rs:386` - unwrap in parsing

**Fix**: Return Result instead of unwrapping.

**Files**:
- `portage-repo/src/make_conf.rs:175,383`
- `portage-repo/src/package_conf.rs:386`

#### 2.4 portage-solver panic!
**Issue**: `src/use_config.rs:382` - panic!("expected owned")

**Fix**: Return proper error.

**Files**: `portage-solver/src/use_config.rs:382`

### Phase 3: Safe Unwraps and Doc Tests (P2) - Week 5

#### 3.1 Safe unwraps
- `portage-cli/src/query/depgraph/output.rs:563` - Use `from_utf8_lossy` instead
- `portage-cli/src/cli.rs` `repo_path()` - `unwrap_or_default()` repo fallbacks (document or propagate)

#### 3.2 Doc test unwraps
Hundreds of `.unwrap()` in doc tests. For library crates, use `// # ` comments to hide from doctest or use `.expect()` with messages.

### Phase 4: Test Code (P3) - Optional
Test code unwraps are acceptable but can be cleaned up for consistency.

---

## Implementation Priority

| Priority | Category | Count | Estimated Effort | Business Impact |
|----------|----------|-------|-----------------|-----------------|
| P0 | Critical user-facing panics | ~30 | 2-3 weeks | High - Prevents crashes |
| P1 | Library panics | ~58 | 2-3 weeks | High - API stability |
| P2 | Safe unwraps & doc tests | ~150 | 1-2 weeks | Medium - Code quality |
| P3 | Test code unwraps | ~100+ | Optional | Low - Consistency |

---

## Tools to Use

1. **clippy**: Run `cargo clippy -- -D warnings` to catch new issues
2. **cargo-test**: Ensure all tests pass after changes
3. **cargo-doc**: Verify documentation builds correctly

---

## Verification Plan

After each phase:
1. Run `cargo build` - must succeed
2. Run `cargo test` - all tests must pass
3. Run `cargo clippy -- -D warnings` - must be warning-free
4. Run `cargo fmt --check` - must be formatted correctly

---

## Success Criteria

1. No `unwrap()` in user-facing code paths (P0)
2. No `panic!()` in library code (P1)
3. All doc tests use proper error handling or are hidden from doctest
4. All tests pass
5. No new clippy warnings introduced

---

## Notes

1. Some unwraps are in `#[test]` functions and are acceptable.
2. Some unwraps are provably safe (e.g., after a `Some` check). These should have `// SAFETY:` comments.
3. The `portage-atom` crate has many `panic!` in `FromStr` implementations. This is a design pattern that should be changed to return `Err` instead.
4. The codebase was largely AI-generated (as noted in AGENTS.md), so expect to find more issues.

---

*Generated: 2026-07-08; refreshed: 2026-07-09*
*Status: Active — P0 backlog in `query/depgraph/` and `package_env.rs`; housekeeping items above marked done*
