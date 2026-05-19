# Plan: Repo Query Commands and Regen

## Phase 1 — Read-only repo queries

- [ ] Task: implement `em query list [pattern]`
    - [ ] Add `src/query/list.rs` — walk repo ebuilds, collect CPVs, filter by pattern
    - [ ] Wire into `run_query` in `main.rs`
    - [ ] Commit: `feat: implement query list`

- [ ] Task: implement `em query which <atom>`
    - [ ] Add `src/query/which.rs` — find best-matching ebuild path for atom
    - [ ] Wire into `run_query`
    - [ ] Commit: `feat: implement query which`

- [ ] Task: implement `em query keywords <atom>`
    - [ ] Add `src/query/keywords.rs` — collect keywords from all versions, render table
    - [ ] Wire into `run_query`
    - [ ] Commit: `feat: implement query keywords`

- [ ] Task: implement `em query uses <atom>`
    - [ ] Add `src/query/uses.rs` — read IUSE from best-matching cache entry
    - [ ] Wire into `run_query`
    - [ ] Commit: `feat: implement query uses`

- [ ] Task: implement `em search <pattern> [--description]`
    - [ ] Add `src/search.rs` — filter CPNs (and optionally DESCRIPTIONs) by pattern
    - [ ] Wire into `run_applet`
    - [ ] Commit: `feat: implement search`

- [ ] Task: implement `em query hasuse <flag>`
    - [ ] Add `src/query/hasuse.rs` — walk all packages, filter by IUSE membership
    - [ ] Wire into `run_query`
    - [ ] Commit: `feat: implement query hasuse`

- [ ] Task: Conductor - User Manual Verification 'Phase 1' (Protocol in workflow.md)

## Phase 2 — Solver-powered reverse deps

- [ ] Task: implement `em query depends <atom>`
    - [ ] Add `src/query/depends.rs` — load full dep graph, collect reverse RDEPEND/DEPEND edges
    - [ ] Wire into `run_query`
    - [ ] Commit: `feat: implement query depends`

- [ ] Task: Conductor - User Manual Verification 'Phase 2' (Protocol in workflow.md)

## Phase 3 — Metadata cache regeneration

- [ ] Task: implement `em regen`
    - [ ] Add `src/regen.rs` — async regen using `portage-repo` EbuildShell, `-j`, `--dedup`, `--output`
    - [ ] Wire into `run_applet`
    - [ ] Commit: `feat: implement regen`

- [ ] Task: Conductor - User Manual Verification 'Phase 3' (Protocol in workflow.md)
