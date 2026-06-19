# Changelog

## 0.8.0

### Breaking changes

- Depend on `portage-atom` 0.10 (which changed `DepEntry::evaluate_use`); the
  major bump is propagated to dependents.

### Features

- Evaluate `REQUIRED_USE` expressions.
- Collect `SRC_URI` distfile names for a given USE state.
- Apply `IUSE` defaults for merge-path parity.
- Expose interned `IUSE` keys without re-interning.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Raise MSRV to 1.92.
