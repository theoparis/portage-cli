# portage-cli (`em`)

## Vision

A fast, correct Rust implementation of the Gentoo Portage command-line interface.
The binary is named `em` and aims to be a drop-in companion to `emerge`, `equery`,
`emaint`, and related tools — sharing their familiar subcommand structure while being
backed by the portage-* crate ecosystem instead of the Python Portage tree.

## Target Users

- Gentoo developers and maintainers who want faster tooling
- Automated CI/CD pipelines that need deterministic package metadata queries
- System administrators maintaining Gentoo installations

## Goals

1. Correct PMS-conformant atom parsing and dependency resolution
2. Fast read-only repository queries (no Python startup overhead)
3. Incremental implementation: ship working subcommands one at a time
4. Interoperability with existing md5-cache and VDB formats

## Current State

- `em atom` — parse and print atoms ✓
- `em query` — all 13 subcommands working (depgraph, belongs, check, depends, files, has, hasuse, keywords, list, meta, size, uses, which) ✓
- `em query depgraph` — full dep resolution via portage-atom-pubgrub ✓
- `em search` — name/description search ✓
- `em regen` — metadata cache regeneration ✓
- `em use` — add/remove USE flags in make.conf ✓
- `em pkg` — edit package.use/keywords/mask/env ✓
- `em ebuild` — fetch, unpack, phases, merge, VDB registration ✓
- `em maint` — world, revisions, moveinst, regen_use ✓
- Default mode (`em <atoms>`) — resolve → build loop ✓
- Remaining stubs: depclean, sync, news, glsa, log, clean, revdep, quickpkg, mirror, grep, select, dispatch, etc, env, portageq
