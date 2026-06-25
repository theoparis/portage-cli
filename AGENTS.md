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

When a commit was significantly assisted by an AI tool, note it with an
`Assisted-by:` trailer rather than a `Co-Authored-By:` trailer. Use the kernel's
format (`AGENT_NAME:MODEL_VERSION`, colon-separated, e.g.
`Assisted-by: Maki:glm-5.2`). Only list *specialized* analysis tools after the
model version if any were used; basic dev tools (git, cargo, editors) are not
listed. The agent never adds a `Signed-off-by` (DCO) — that is the human's.

## MSRV

Until the first complete release, the workspace tracks **latest stable**
dependencies and bumps `rust-version` as needed (currently **1.92**, driven by
`pubgrub` 0.4). Do not pin crates to older releases to satisfy a lower MSRV.

CI runs `stable` and the declared workspace minimum (`1.92`). After a release,
foundational crates may again advertise a lower standalone MSRV; the workspace
floor follows whatever latest deps require.

When a dependency bump needs a newer compiler, raise `rust-version` in every
affected `Cargo.toml` and the CI matrix entry, then `cargo msrv verify`.

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising behaviour. Do not assume existing patterns are
correct.
