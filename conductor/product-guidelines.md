# Product Guidelines

## CLI UX Principles

- Mirror existing Portage tool conventions (flags, subcommand names, output format)
  so muscle memory transfers directly
- Errors go to stderr; data output goes to stdout — always pipeable
- Machine-readable output where it makes sense (one item per line)
- Silent on success unless `-v`; no progress spinners in non-TTY contexts
- Exit codes: 0 success, 1 user error, 2 internal/not-implemented

## Output Style

- Package identifiers always as `category/name-version` (CPV form) unless the
  context only needs a name
- Keywords displayed as a compact table, one arch per column
- USE flags: prefix `+` for enabled default, `-` for disabled default, plain for unset
- Dependency edges: `from --> to [CLASS]` for graph output

## Code Style

- No `unwrap()` in user-facing paths — propagate errors via `Result`
- Prefer `portage-atom` types over raw strings wherever possible
- Keep each applet in its own module under `src/`
- One commit per working applet or subcommand
