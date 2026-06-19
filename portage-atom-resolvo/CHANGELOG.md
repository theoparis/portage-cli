# Changelog

## 0.7.0

### Breaking changes

- Depend on `portage-atom` 0.10; the major bump is propagated to dependents.

### Features

- Solver-decided USE flags via `virtual/USE_<flag>` solvables and conditions.
- Blocker handling through inverted version constraints.
- Improved autounmask `package.use` writing.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Point `repository` at the workspace and add `keywords`/`categories`.
- Raise MSRV to 1.92.
