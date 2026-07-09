# portage-distfiles

Source distfile fetching and resolution for Gentoo Portage.

Resolves `SRC_URI` entries to mirror URLs, downloads distfiles honoring
`DISTDIR`/`PORTAGE_RO_DISTDIRS`, and verifies them against `Manifest` checksums.
Used by the [`em`](https://github.com/lu-zero/portage-cli) ebuild fetch phase.

## Overview

- `DistfileResolver` — expand `SRC_URI` to `Distfile` structs with mirror lists
- `Fetcher` — download distfiles (builtin HTTP or external command)
- `MirrorList` — Gentoo mirror configuration
- `fetch_binpkg` / `fetch_index` — binhost transport helpers

Workspace crate (`publish = false`); not yet on crates.io.

## License

MIT