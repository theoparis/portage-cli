# Benchmark Machines

This directory contains one `.md` file per physical/logical machine used for benchmarks. Each file includes:

- Full hardware characterization (lscpu, numactl, memory, etc.)
- NUMA topology and binding recommendations (important for servers)
- Freq/scaling notes
- Available Gentoo trees at time of runs
- Re-characterization commands (run these for every new session)
- Notes on comparability to other machines

## Current Machines

- [thalia](thalia.md): Primary development server (AmpereOne, 128 cores, 4 NUMA, 256 GiB). Use for new AmpereOne numbers.
- [mneme](mneme.md): Apple M2 Max (laptop, named "mneme"). Historical data present; **fresh benchmark runs (including cache regen comparisons and dep resolution parity/timing) planned later** per "we'll run the benchmarks on the m2 later". The file is prepped with detailed macOS/M2 repro steps, characterization, and notes.

## Usage in Blogpost / Reports

- Always run the re-char commands and include/link the machine .md.
- Cross-reference in tables: e.g. "Regen times on thalia (see machines/thalia.md)"
- Old M2 Max numbers should note they are from different hardware class (laptop UMA vs server NUMA).
- For comparative regen (em / pkgcraft pk / egencache) and dep resolution (emerge -p vs em -p): see instructions and collected data in machines/mneme.md (and thalia.md for server side). Use the scripts/compare-regen.sh and bench-em-vs-emerge.sh.
- Note that many benchmark bash scripts live in the specific crates (portage-repo/, root-level bench-regen.sh, etc.). machines/mneme.md now collects descriptions of all of them for running on mneme.

See parent `BENCHMARKS.md` for how results tie back to machines.
