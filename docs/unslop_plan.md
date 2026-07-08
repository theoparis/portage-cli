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

## Legend

- **P0 (Critical)**: User-facing panics that can crash the application during normal operations
- **P1 (High)**: Library panics that could affect downstream users
- **P2 (Medium)**: Provably safe unwraps that should still use proper error handling
- **P3 (Low)**: Test code unwraps (acceptable but should be consistent)

---

## Summary Statistics

| Crate | unwrap() | expect() | panic! | Total |
|-------|----------|----------|--------|-------|
| portage-cli | ~150+ | ~20+ | ~5+ | 175+ |
| portage-atom | ~100+ | ~5+ | ~40+ | 145+ |
| portage-metadata | ~80+ | ~5+ | ~2+ | 87+ |
| portage-repo | ~60+ | ~10+ | ~5+ | 75+ |
| portage-solver | ~10+ | 0 | ~2+ | 12+ |
| gentoo-core | ~30+ | ~2+ | ~4+ | 36+ |
| **Total** | **~430+** | **~42+** | **~58+** | **~530+** |

*Numbers are approximate based on grep results*

---

## Detailed Findings by Crate

### portage-cli Crate (Binary)

#### Critical Issues (P0)

##### `src/main.rs`
- Line 88, 92: `stack_holder.as_ref().unwrap()` - **SAFE** with comment explaining safety
- Line 151: `.expect("failed to build the tokio runtime")` - Runtime initialization, should be proper error

##### `src/cli.rs`
- Line 335: `.expect("prefix path conversion should succeed when prefix.is_some()")` - P0
- Line 418, 420: `.unwrap()` in assertions (test code?)
- Line 459, 486: `.unwrap()` on repo paths - P0

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
- Line 386: `PackageConf::parse(s.to_owned()).unwrap()`
- Lines 393, 402, 410, 429, 445, 459, 468, 477: `.unwrap()` in test code

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

#### 1.4 portage-cli/src/cli.rs
**Issues**:
- Line 335: `.expect("prefix path conversion should succeed when prefix.is_some()")`

**Fix**:
```rust
// If this is in a context where prefix.is_some() is guaranteed, add a debug_assert
// Otherwise, handle the error properly
```

**Files**: `src/cli.rs:335`

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
- `portage-cli/src/main.rs:88,92` - Add better SAFETY comments
- `portage-cli/src/query/depgraph/output.rs:563` - Use `from_utf8_lossy` instead

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

*Generated: 2026-07-08*
*Status: Draft - Requires review and prioritization*
