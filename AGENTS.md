# Project Conventions

## Build Commands

```bash
cargo build                        # Build the em binary
cargo test                         # Run all tests
cargo clippy -- -D warnings        # Lint — must be warning-free
cargo fmt --check                  # Format check — must pass
```

## Architecture

- Binary crate producing the `em` command; CLI built with
  [clap](https://crates.io/crates/clap) derive macros, subcommands of the
  top-level `Cli` struct. Keep `main.rs` thin; extract modules as complexity grows.
- Business logic is delegated to the library crates (`portage-atom`,
  `portage-metadata`, `portage-repo`, `portage-atom-pubgrub`, …).
- **Read [`docs/architecture.md`](./docs/architecture.md) first** — it is the
  main architecture reference (the `em -p` resolution pipeline, USE stacking
  precedence, the USE/solver boundary, post-solve validation, and known
  divergences from emerge). Keep it updated as the design changes.
- [`ARCHITECTURE.md`](./ARCHITECTURE.md) is the per-crate public-API catalog and
  publishing status.

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

Minimum Supported Rust Version is **1.85** (edition 2024).

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising behaviour. Do not assume existing patterns are
correct.
