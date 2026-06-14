# Project Conventions

## Build Commands

```bash
cargo build                        # Build the em binary
cargo test                         # Run all tests
cargo clippy -- -D warnings        # Lint — must be warning-free
cargo fmt --check                  # Format check — must pass

# MSRV verification (use the project's cargo-msrv tool)
cargo install cargo-msrv
cargo msrv verify                  # Verifies the rust-version declared in Cargo.toml
# For a specific crate or version:
# cargo msrv verify --manifest-path portage-cli/Cargo.toml
# cargo msrv verify --rust-version 1.88
```

## Architecture

- Binary crate producing the `em` command; CLI built with
  [clap](https://crates.io/crates/clap) derive macros, subcommands of the
  top-level `Cli` struct. Keep `main.rs` thin; extract modules as complexity grows.
- Business logic is delegated to the library crates (`portage-atom`,
  `portage-metadata`, `portage-repo`, `portage-atom-pubgrub`, …).
- **Read [`docs/architecture.md`](./docs/architecture.md) first** — it is the
  main architecture reference (crate catalog, the `em -p` resolution pipeline,
  USE stacking precedence, the USE/solver boundary, post-solve validation, and
  known divergences from emerge). Keep it updated as the design changes.

## Dependencies

- `portage-atom` — PMS atom parsing (Cpn, Cpv, Dep, etc.)
- `portage-metadata` — md5-cache metadata, `RequiredUseExpr`, keywords, IUSE
- `portage-repo` — repository layout, profile stack, embedded ebuild shell
- `portage-atom-pubgrub` — the PubGrub solver bridge `em` resolves through
- `clap` — CLI argument parsing
- `tokio` — async runtime
- `thiserror` — error derive macros

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types and functions
- Tests live in a `#[cfg(test)] mod tests` block

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` — new functionality
- `fix:` — bug fix
- `refactor:` — code restructuring without behaviour change
- `docs:` — documentation only
- `test:` — adding or updating tests
- `ci:` — CI/CD changes
- `chore:` — maintenance (dependencies, tooling)

## MSRV

**Different crates report different MSRV**:

- Foundational crates (`portage-atom`, `gentoo-interner`, `portage-atom-resolvo`)
  target **1.85** (edition 2024) and can be used standalone at that version.
- The workspace as a whole, the `em` CLI (`portage-cli`), `portage-repo`, and
  any crate that depends on `portage-repo` (directly or transitively) target
  **1.88** (edition 2024). This is dictated by the pinned brush fork
  (`brush-core` etc. declare 1.88.0) used for ebuild sourcing.

We are fine with these minimums; they have been verified with `cargo msrv verify`
(reading the `rust-version` from each `Cargo.toml`) as well as direct toolchain
checks. CI tests the workspace minimum (1.88 + stable).

When editing, do not introduce features requiring a newer Rust without bumping
the relevant `rust-version` (and the CI matrix entry if it affects the
workspace), then re-verify with `cargo msrv verify --manifest-path <path>`.

See the Build Commands section above for the recommended `cargo msrv` usage.
(We do not bisect further with `cargo msrv find` unless a specific need arises.)

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising behaviour. Do not assume existing patterns are
correct.
