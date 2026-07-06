/// Merge-behavior flags: everything `emerge_atoms`/`emerge_atoms_inner`/
/// `run_merge_plan` read to decide *how* to resolve and build a set of atoms,
/// as opposed to root-model flags (`--root`, `--local`, `--privilege`, …,
/// already `global = true` on [`super::Cli`] since they're meaningful to
/// every applet) or depgraph-shape flags ([`super::DepgraphFlags`]: `--deep`/
/// `--newuse`).
///
/// Flattened both into the top-level [`super::Cli`] (for the bare `em
/// <atoms>` path) and into [`super::ToolchainArgs`]/[`super::CrossdevArgs`]/
/// [`super::StagesArgs`] (whose staged driver, `crossdev::run_staged`, calls
/// the very same `emerge_atoms`/`emerge_atoms_inner` chain per step) —
/// mirroring exactly how [`super::DepgraphFlags`] is already flattened in
/// both places. This lets these flags be written either before or after the
/// subcommand name (`em -j 80 stages --stage1` or `em stages --stage1 -j
/// 80`), each populating its own instance; the driver merges the two with
/// the same precedence (subcommand value wins when set, falling back to the
/// global one — the same precedence
/// `merge_depgraph_flags` already uses).
///
/// `--search`/`--searchdesc` are deliberately NOT here: they select an
/// entirely different mode in the bare path (`run_emerge` branches to
/// `search::run_emerge_style` before ever calling `emerge_atoms`), so they
/// have no meaning for a subcommand's staged build. `--nodeps` is also NOT
/// here: it is already threaded explicitly per call
/// ([`crate::EmergeOpts::nodeps`]) because each [`crate::crossdev::stages::StageStep`] needs
/// its own value (the two-stage cross bootstrap's `--nodeps` libc-headers
/// step), not a single global/per-invocation one — folding it into this
/// mixin would lose that per-step distinction.
///
/// Found 2026-07-03 running `em stages --stage1 -j 80 --keep-going`: `-j`/
/// `--keep-going`/`--autosolve-use`/`--autounmask-write` all parsed only
/// when placed *before* the subcommand (clap rejects non-global args placed
/// after one), and `run_staged`'s driver read them straight off the
/// top-level `Cli` regardless of where `stages`/`crossdev`/`toolchain`'s own
/// flattened copy might set them — so a flag given *after* the subcommand
/// silently had no effect even where clap did accept it. See
/// `todo/stage-build-shakeout.md`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct MergeFlags {
    #[arg(short = 'u', long)]
    pub update: bool,

    /// Write required USE changes to /etc/portage/package.use/
    #[arg(long)]
    pub autounmask_write: bool,

    #[arg(short = '1', long = "oneshot")]
    pub oneshot: bool,

    #[arg(short = 'f', long)]
    pub fetchonly: bool,

    #[arg(short = 'b', long)]
    pub buildpkg: bool,

    #[arg(short = 'k', long)]
    pub usepkg: bool,

    #[arg(short = 'K', long)]
    pub usepkgonly: bool,

    #[arg(short = 'g', long)]
    pub getbinpkg: bool,

    #[arg(short = 'G', long)]
    pub getbinpkgonly: bool,

    #[arg(short = 'e', long)]
    pub emptytree: bool,

    // No short alias: `-t` collides with `em crossdev`'s `--target` once
    // MergeFlags is flattened into CrossdevArgs (clap's debug_assertions catch
    // this in dev builds; release builds skip the check, so it was silently
    // latent). Real emerge has no short form for --tree either.
    #[arg(long)]
    pub tree: bool,

    /// Emit the depgraph as machine-parsable JSON instead of pretend text.
    /// Takes precedence over `--tree`. Works with `-p` (including `-e`).
    #[arg(long)]
    pub json: bool,

    #[arg(short = 'o', long)]
    pub onlydeps: bool,

    #[arg(short = 'n', long)]
    pub noreplace: bool,

    /// Build up to N packages in parallel, respecting build-dependency order
    /// (merges are still serialised). Default 1 (sequential).
    #[arg(short = 'j', long, value_name = "N")]
    pub jobs: Option<u32>,

    #[arg(short = 'l', long, value_name = "LOAD")]
    pub load_average: Option<f64>,

    #[arg(long)]
    pub keep_going: bool,

    #[arg(long)]
    pub autounmask: bool,

    /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C) rather
    /// than only reporting violations. Off by default; flips are reported.
    #[arg(long)]
    pub autosolve_use: bool,

    #[arg(long)]
    pub complete_graph: bool,

    /// Include build-time dependencies (BDEPEND) in the resolution.
    /// Default is false (exclude BDEPEND), matching emerge's default.
    /// When enabled, BDEPEND are included but filtered by what's already
    /// installed on the build host (BROOT).
    #[arg(long)]
    pub with_bdeps: bool,

    #[arg(short = 'X', long, value_name = "ATOM")]
    pub exclude: Vec<String>,

    /// emerge's `--root-deps[=rdeps]`: only RDEPEND (not DEPEND) is required to
    /// be satisfied in the merge target — a work-around for the crossdev
    /// bootstrap cycle (a still-empty target sysroot can't yet satisfy plain
    /// DEPEND, e.g. `virtual/os-headers`/`acct-group/root`, while its own
    /// toolchain is being built into it). `em crossdev --setup` always applies
    /// this unconditionally (matching crossdev's `<CTARGET>-emerge` wrapper),
    /// regardless of this flag. Elsewhere (bare `em`, `em toolchain --setup`,
    /// `em stages --stage1`, `equery depgraph`) it defaults off: once a
    /// target's toolchain already exists, ordinary packages should have their
    /// full DEPEND resolved against the target like any native build — this
    /// flag lets that be overridden case by case. See
    /// `todo/stage-build-shakeout.md`.
    #[arg(long = "root-deps")]
    pub root_deps: bool,
}
