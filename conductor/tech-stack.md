# Tech Stack

## Language

Rust (edition 2024, MSRV 1.92)

## Key Dependencies

| Crate | Role |
|-------|------|
| `portage-atom` | Atom/dep parsing, CPV/CPN types, Version |
| `portage-metadata` | md5-cache entry parsing, IUSE, Keywords, Slots |
| `portage-repo` | Repository layout, profile stack, EbuildShell |
| `portage-atom-pubgrub` | PubGrub-based dependency resolver |
| `gentoo-core` | Architecture types |
| `clap` (derive) | CLI argument parsing |
| `tokio` (rt-multi-thread) | Async runtime (required by EbuildShell) |
| `thiserror` | Error types |
| `itertools` | Iterator helpers |

## Architecture

Single binary (`em`) with a `clap` subcommand tree.  Each applet lives in its own
module under `src/`.  Async is used only where portage-repo requires it (ebuild
sourcing); query-only commands run synchronously.

## Repository Layout

```
src/
  main.rs          — CLI dispatch
  cli.rs           — clap structs
  error.rs         — Error / Result types
  depgraph.rs      — query depgraph implementation
  <applet>.rs      — one module per implemented applet
```

## Data Sources

- **md5-cache** at `<repo>/metadata/md5-cache/` — read via `portage-repo`
- **metadata.xml** at `<repo>/<cat>/<pkg>/metadata.xml` — XML, parsed inline
- **VDB** at `/var/db/pkg/` — not yet implemented
