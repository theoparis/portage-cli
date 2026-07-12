# Project Conventions

## Build Commands

```bash
cargo build                        # Build the em binary
cargo test --workspace --exclude portage-bench
cargo clippy --workspace --exclude portage-bench -- -D warnings
cargo fmt --all -- --check

# MSRV verification (use the project's cargo-msrv tool)
cargo install cargo-msrv
cargo msrv verify --rust-version 1.95 --path portage-cli
```

## Architecture

- Binary crate producing the `em` command; CLI built with
  [clap](https://crates.io/crates/clap) derive macros, subcommands of the
  top-level `Cli` struct. Keep `main.rs` thin; extract modules as complexity grows.
- Business logic is delegated to the library crates (`portage-atom`,
  `portage-metadata`, `portage-solver`, `portage-repo`, `portage-atom-pubgrub`,
  `portage-vdb`, `portage-binpkg`, `portage-distfiles`, …).
- **Read [`docs/architecture.md`](./docs/architecture.md) first** — it is the
  main architecture reference (crate catalog, the `em -p` resolution pipeline,
  USE stacking precedence, the USE/solver boundary, post-solve validation, and
  known divergences from emerge). Keep it updated as the design changes.

## Dependencies

Workspace members (14 crates + `portage-bench`):

- `gentoo-interner` — string interning
- `gentoo-core` — architecture and variant types
- `portage-atom` — PMS atom parsing (Cpn, Cpv, Dep, etc.)
- `portage-metadata` — md5-cache metadata, `RequiredUseExpr`, keywords, IUSE
- `portage-solver` — solver-agnostic trait and shared vocabulary
- `portage-atom-pubgrub` — PubGrub solver bridge (`em` resolves through this by default)
- `portage-atom-resolvo` — Resolvo SAT solver bridge (cross-check)
- `portage-repo` — repository layout, profile stack, embedded ebuild shell
- `portage-vdb` — installed package database (`/var/db/pkg`)
- `portage-binpkg` — GPKG binary package read/write
- `portage-distfiles` — distfile fetch and mirror resolution
- `gentoo-stages` — stage3 tarball fetch/cache
- `portage-cli` — the `em` binary (unpublished)
- `portage-bench` — benchmark harness (excluded from CI)

CLI/runtime deps: `clap`, `tokio`, `anyhow`, `thiserror`.

## Local dependency overrides

Machine-specific `[patch.crates-io]` paths in `.cargo/config.toml` (for local
`brush`/`pkgcraft` worktrees) are expected during development and are gitignored.
Do not commit them.

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
dependencies and bumps `rust-version` as needed (currently **1.95**, driven by
`cfg_select!` stabilization). Do not pin crates to older releases to satisfy a
lower MSRV.

CI runs `stable` and the declared workspace minimum (`1.95`). After a release,
foundational crates may again advertise a lower standalone MSRV; the workspace
floor follows whatever latest deps require.

When a dependency bump needs a newer compiler, raise `rust-version` in
`[workspace.package]` and the CI matrix entry, then `cargo msrv verify`.

## Testing strategy

See [`docs/testing.md`](./docs/testing.md) for the full picture: why
`cargo nextest` is preferred locally over plain `cargo test` (known
`portage-repo` flakiness), the live-parity-against-real-`emerge` workflow
that has caught most of this project's real bugs, and the pre-PR checklist.

## Gentoo host tests

Five integration tests in `portage-cli/tests/comparison.rs` compare `em query`
output against `qfile`/`qlist` and are `#[ignore]` by default. On a Gentoo host:

```bash
cargo test -p portage-cli -- --ignored
```

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising behaviour. Do not assume existing patterns are
correct.