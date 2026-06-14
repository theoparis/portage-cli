//! `do*` install helpers as Rust builtins (PMS 12.3.x).
//!
//! These replace the hand-parsed bash versions in `INSTALL_HELPERS`: real clap
//! arg parsing (`-r`, `-x`), `${ED}`/dest-tree awareness, and the shared
//! [`DieFlag`](super::die::DieFlag) on failure. They shell out to coreutils
//! `install`/`cp`/`ln` for byte-identical mode/ownership semantics — the win is
//! removing fragile bash, not reimplementing install(1).
//!
//! The `new*` helpers stay in `INSTALL_HELPERS` as thin wrappers: they stage the
//! source as `${T}/$2` (handling the `-` = stdin convention) and then call the
//! matching `do*` builtin here, so there is a single install path. The pure
//! destination-state setters (`into`/`insinto`/…) also stay in bash.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;

/// The image staging root files install into: `${ED}` (= `${D}${EPREFIX}`),
/// trailing slash trimmed. Falls back to `${D}` then `/` (should not happen
/// once `init_build_env` has run).
fn ed<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>) -> PathBuf {
    for var in ["ED", "D"] {
        if let Some(v) = shell.env_str(var).filter(|s| !s.is_empty()) {
            return PathBuf::from(v.trim_end_matches('/'));
        }
    }
    PathBuf::from("")
}

fn var<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    name: &str,
    default: &str,
) -> String {
    shell
        .env_str(name)
        .map(|s| s.into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// `${ED}` joined with `rel` (a destination path), leading slash stripped so the
/// join stays under `${ED}` rather than resetting to an absolute path.
fn under_ed(ed: &Path, rel: &str) -> PathBuf {
    ed.join(rel.trim_start_matches('/'))
}

/// PMS `get_libdir`: `LIBDIR_${ABI}` if multilib, else `CONF_LIBDIR`, else `lib`.
fn get_libdir<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>) -> String {
    let abi = var(shell, "ABI", "");
    if !abi.is_empty() {
        let v = var(shell, &format!("LIBDIR_{abi}"), "");
        if !v.is_empty() {
            return v;
        }
    }
    let conf = var(shell, "CONF_LIBDIR", "");
    if !conf.is_empty() {
        return conf;
    }
    "lib".to_string()
}

/// Raise the shared die flag with `msg` and return exit status 1, matching the
/// bash helpers' `|| die "..."`.
fn raise_die<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
    msg: &str,
) -> brush_core::ExecutionResult {
    if let Ok(flag) = context.shared::<DieFlag>() {
        flag.raise(msg);
    }
    let _ = writeln!(context.params.stderr(context.shell), "die: {msg}");
    brush_core::ExecutionResult::new(1)
}

/// Run blocking filesystem work off the async runtime, mapping its `Result` onto
/// success / `die`. Spawned `install`/`cp`/`ln` inherit em's stdio (silent on
/// success); failures surface through the returned message via `raise_die`.
async fn run_blocking<SE, F>(
    context: &brush_core::ExecutionContext<'_, SE>,
    f: F,
) -> brush_core::ExecutionResult
where
    SE: brush_core::ShellExtensions,
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    match tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(format!("task failed: {e}")))
    {
        Ok(()) => brush_core::ExecutionResult::success(),
        Err(msg) => raise_die(context, &msg),
    }
}

// ---- blocking primitives (run inside `run_blocking`'s closure) ----

fn mkdir_p(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))
}

fn ok_status(
    st: std::io::Result<std::process::ExitStatus>,
    verb: &str,
    what: &Path,
) -> Result<(), String> {
    match st {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("failed to {verb} {}", what.display())),
    }
}

fn install_file(
    env: &[(String, String)],
    opts: &[String],
    src: &Path,
    dest: &Path,
) -> Result<(), String> {
    let mut c = Command::new("install");
    for o in opts {
        if !o.is_empty() {
            c.arg(o);
        }
    }
    let st = c.arg(src).arg(dest).envs(env.iter().cloned()).status();
    ok_status(st, "install", src)
}

fn cp_recursive(env: &[(String, String)], src: &Path, destdir: &Path) -> Result<(), String> {
    let st = Command::new("cp")
        .arg("-pPR")
        .arg(src)
        .arg(format!("{}/", destdir.display()))
        .envs(env.iter().cloned())
        .status();
    ok_status(st, "copy", src)
}

fn basename(p: &Path) -> std::ffi::OsString {
    p.file_name().map(|n| n.to_owned()).unwrap_or_default()
}

/// `os.path.relpath(target, dirname(link))` for the absolute paths `dosym -r`
/// uses (both rooted at `/`). Falls back to `target` verbatim if either is
/// relative (uncommon for `dosym -r`).
fn relpath(target: &str, link: &str) -> String {
    let start = link.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    if !target.starts_with('/') || !start.starts_with('/') {
        return target.to_string();
    }
    let t: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = start.split('/').filter(|s| !s.is_empty()).collect();
    let common = t.iter().zip(s.iter()).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; s.len() - common];
    parts.extend(t[common..].iter().copied());
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

// ---- destination-directory helpers ----

/// `dodir <dir>...` (PMS 12.3.1): create directories under `${ED}`.
#[derive(Parser)]
pub(crate) struct DodirCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    dirs: Vec<String>,
}

impl builtins::Command for DodirCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let ed = ed(context.shell);
        let dirs = self.dirs.clone();
        Ok(run_blocking(&context, move || {
            for d in &dirs {
                mkdir_p(&under_ed(&ed, d)).map_err(|e| format!("dodir: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

/// `keepdir <dir>...` (PMS 12.3.1): `dodir` plus a `.keep_*` marker so the empty
/// directory survives image merging.
#[derive(Parser)]
pub(crate) struct KeepdirCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    dirs: Vec<String>,
}

impl builtins::Command for KeepdirCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let ed = ed(context.shell);
        let category = var(context.shell, "CATEGORY", "");
        let pn = var(context.shell, "PN", "");
        let slot = var(context.shell, "SLOT", "").replace('/', "_");
        let dirs = self.dirs.clone();
        Ok(run_blocking(&context, move || {
            for d in &dirs {
                let dir = under_ed(&ed, d);
                mkdir_p(&dir).map_err(|e| format!("keepdir: {e}"))?;
                let keep = dir.join(format!(".keep_{category}_{pn}-{slot}"));
                std::fs::File::create(&keep)
                    .map_err(|e| format!("keepdir: creating {}: {e}", keep.display()))?;
            }
            Ok(())
        })
        .await)
    }
}

// ---- single-destination doers (install list of files into one dir) ----

/// Build the `doX`-style "install these files into `dest`" closure body shared by
/// the simple doers. `recursive` enables `-r` (copy directories wholesale).
fn install_files_closure(
    helper: &'static str,
    env: Vec<(String, String)>,
    opts: Vec<String>,
    dest: PathBuf,
    cwd: PathBuf,
    files: Vec<String>,
    recursive: bool,
) -> impl FnOnce() -> Result<(), String> + Send + 'static {
    move || {
        mkdir_p(&dest).map_err(|e| format!("{helper}: {e}"))?;
        for f in &files {
            let src = cwd.join(f);
            if recursive && src.is_dir() {
                cp_recursive(&env, &src, &dest).map_err(|e| format!("{helper}: {e}"))?;
            } else {
                let target = dest.join(basename(&src));
                install_file(&env, &opts, &src, &target).map_err(|e| format!("{helper}: {e}"))?;
            }
        }
        Ok(())
    }
}

macro_rules! require_files {
    ($ctx:expr, $files:expr, $helper:literal) => {
        if $files.is_empty() {
            return Ok(raise_die(
                &$ctx,
                concat!($helper, ": at least one argument required"),
            ));
        }
    };
}

/// `doins [-r] <file>...` (PMS 12.3.4): install into `${ED}${INSDESTTREE}` with
/// `INSOPTIONS` (default `-m0644`); `-r` recurses into directories.
#[derive(Parser)]
pub(crate) struct DoinsCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoinsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doins");
        let env = super::context_env(&context);
        let insdest = var(context.shell, "INSDESTTREE", "");
        let opts: Vec<String> = var(context.shell, "_insopts", "-m0644")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let dest = under_ed(&ed(context.shell), &insdest);
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "doins",
                env,
                opts,
                dest,
                cwd,
                self.files.clone(),
                self.recursive,
            ),
        )
        .await)
    }
}

/// `doexe <file>...` (PMS 12.3.4): install into `${ED}${EXEDESTTREE}` with
/// `EXEOPTIONS` (default `-m0755`).
#[derive(Parser)]
pub(crate) struct DoexeCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoexeCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doexe");
        let env = super::context_env(&context);
        let exedest = var(context.shell, "EXEDESTTREE", "");
        let opts: Vec<String> = var(context.shell, "_exeopts", "-m0755")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let dest = under_ed(&ed(context.shell), &exedest);
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure("doexe", env, opts, dest, cwd, self.files.clone(), false),
        )
        .await)
    }
}

/// `dobin <file>...` (PMS 12.3.4): install executables into `${DESTTREE}/bin`
/// (mode 0755).
#[derive(Parser)]
pub(crate) struct DobinCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DobinCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dobin");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let dest = under_ed(&ed(context.shell), &format!("{into}/bin"));
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "dobin",
                env,
                vec!["-m0755".into()],
                dest,
                cwd,
                self.files.clone(),
                false,
            ),
        )
        .await)
    }
}

/// `dosbin <file>...` (PMS 12.3.4): install executables into `${DESTTREE}/sbin`
/// (mode 0755).
#[derive(Parser)]
pub(crate) struct DosbinCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DosbinCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dosbin");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let dest = under_ed(&ed(context.shell), &format!("{into}/sbin"));
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "dosbin",
                env,
                vec!["-m0755".into()],
                dest,
                cwd,
                self.files.clone(),
                false,
            ),
        )
        .await)
    }
}

/// `dodoc [-r] <file>...` (PMS 12.3.4): install docs into
/// `${ED}/usr/share/doc/${PF}[/${DOCDESTTREE}]` (mode 0644).
#[derive(Parser)]
pub(crate) struct DodocCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DodocCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dodoc");
        let env = super::context_env(&context);
        let pf = var(context.shell, "PF", "");
        let docdesttree = var(context.shell, "DOCDESTTREE", "");
        let sub = if docdesttree.is_empty() {
            format!("usr/share/doc/{pf}")
        } else {
            format!("usr/share/doc/{pf}/{docdesttree}")
        };
        let dest = under_ed(&ed(context.shell), &sub);
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "dodoc",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                self.recursive,
            ),
        )
        .await)
    }
}

/// `doheader [-r] <file>...` (PMS 12.3.4): install headers into
/// `${ED}/usr/include` (mode 0644).
#[derive(Parser)]
pub(crate) struct DoheaderCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoheaderCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doheader");
        let env = super::context_env(&context);
        let dest = under_ed(&ed(context.shell), "usr/include");
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "doheader",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                self.recursive,
            ),
        )
        .await)
    }
}

/// `doinfo <file>...` (PMS 12.3.4): install GNU info files into
/// `${ED}/usr/share/info` (mode 0644).
#[derive(Parser)]
pub(crate) struct DoinfoCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoinfoCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doinfo");
        let env = super::context_env(&context);
        let dest = under_ed(&ed(context.shell), "usr/share/info");
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "doinfo",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                false,
            ),
        )
        .await)
    }
}

/// `doman [-i18n=<locale>] <file>...` (PMS 12.3.4): install man pages into
/// `${ED}/usr/share/man/man<section>`, section taken from each file's extension.
#[derive(Parser)]
pub(crate) struct DomanCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DomanCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doman");
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            for f in &files {
                let src = cwd.join(f);
                let name = basename(&src);
                let ext = Path::new(&name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .filter(|e| !e.is_empty())
                    .ok_or_else(|| format!("doman: cannot determine man section for {f}"))?;
                let dest = under_ed(&ed, &format!("usr/share/man/man{ext}"));
                mkdir_p(&dest).map_err(|e| format!("doman: {e}"))?;
                install_file(&env, &["-m0644".into()], &src, &dest.join(&name))
                    .map_err(|e| format!("doman: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

/// `domo <file>...` (PMS 12.3.4): install `.mo` files into
/// `${ED}/usr/share/locale/<locale>/LC_MESSAGES/${MOPREFIX:-${PN}}.mo`.
#[derive(Parser)]
pub(crate) struct DomoCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DomoCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "domo");
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let pn = var(context.shell, "PN", "");
        let moprefix = var(context.shell, "MOPREFIX", &pn);
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            for f in &files {
                let src = cwd.join(f);
                let stem = Path::new(&basename(&src))
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dest = under_ed(&ed, &format!("usr/share/locale/{stem}/LC_MESSAGES"));
                mkdir_p(&dest).map_err(|e| format!("domo: {e}"))?;
                install_file(
                    &env,
                    &["-m0644".into()],
                    &src,
                    &dest.join(format!("{moprefix}.mo")),
                )
                .map_err(|e| format!("domo: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

// ---- library helpers ----

/// `dolib.a <file>...` (PMS 12.3.4): install static libs into
/// `${DESTTREE}/$(get_libdir)` (mode 0644).
#[derive(Parser)]
pub(crate) struct DolibaCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DolibaCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dolib.a");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let libdir = get_libdir(context.shell);
        let dest = under_ed(&ed(context.shell), &format!("{into}/{libdir}"));
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "dolib.a",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                false,
            ),
        )
        .await)
    }
}

/// `dolib.so <file>...` (PMS 12.3.4): install shared libs into
/// `${DESTTREE}/$(get_libdir)` (mode 0755).
#[derive(Parser)]
pub(crate) struct DolibsoCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DolibsoCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dolib.so");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let libdir = get_libdir(context.shell);
        let dest = under_ed(&ed(context.shell), &format!("{into}/{libdir}"));
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "dolib.so",
                env,
                vec!["-m0755".into()],
                dest,
                cwd,
                self.files.clone(),
                false,
            ),
        )
        .await)
    }
}

/// `dolib <file>...` (PMS 12.3.4): dispatch each file to `dolib.so`/`dolib.a` by
/// suffix (`.so`/`.so.*` are shared libraries, mode 0755; rest are 0644).
#[derive(Parser)]
pub(crate) struct DolibCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DolibCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dolib");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let libdir = get_libdir(context.shell);
        let dest = under_ed(&ed(context.shell), &format!("{into}/{libdir}"));
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            mkdir_p(&dest).map_err(|e| format!("dolib: {e}"))?;
            for f in &files {
                let src = cwd.join(f);
                let name = basename(&src);
                let n = name.to_string_lossy();
                let mode = if n.ends_with(".so") || n.contains(".so.") {
                    "-m0755"
                } else {
                    "-m0644"
                };
                install_file(&env, &[mode.into()], &src, &dest.join(&name))
                    .map_err(|e| format!("dolib: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

// ---- symlinks, perms, ownership ----

/// `dosym [-r] <target> <link>` (PMS 12.3.4): create a symlink at `${ED}${link}`;
/// `-r` makes `target` relative to the link's directory.
#[derive(Parser)]
pub(crate) struct DosymCommand {
    #[arg(short = 'r')]
    relative: bool,
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for DosymCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.args.len() != 2 {
            return Ok(raise_die(&context, "dosym: usage: dosym [-r] target link"));
        }
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let target = self.args[0].clone();
        let link = self.args[1].clone();
        let relative = self.relative;
        Ok(run_blocking(&context, move || {
            let link_dir = link.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            mkdir_p(&under_ed(&ed, link_dir)).map_err(|e| format!("dosym: {e}"))?;
            let resolved = if relative {
                relpath(&target, &link)
            } else {
                target.clone()
            };
            let link_path = under_ed(&ed, &link);
            let st = Command::new("ln")
                .arg("-snf")
                .arg(&resolved)
                .arg(&link_path)
                .envs(env.iter().cloned())
                .status();
            ok_status(st, "symlink", &link_path).map_err(|e| format!("dosym: {e}"))
        })
        .await)
    }
}

/// `fperms <mode> <file>...` (PMS 12.3.5): `chmod` paths relative to `${ED}`.
#[derive(Parser)]
pub(crate) struct FpermsCommand {
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for FpermsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.args.len() < 2 {
            return Ok(raise_die(&context, "fperms: usage: fperms mode file..."));
        }
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let mode = self.args[0].clone();
        let files = self.args[1..].to_vec();
        Ok(run_blocking(&context, move || {
            for f in &files {
                let path = under_ed(&ed, f);
                let st = Command::new("chmod")
                    .arg(&mode)
                    .arg(&path)
                    .envs(env.iter().cloned())
                    .status();
                ok_status(st, "chmod", &path).map_err(|e| format!("fperms: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

/// `fowners <owner>[:<group>] <file>...` (PMS 12.3.5): `chown` paths relative to
/// `${ED}`.
#[derive(Parser)]
pub(crate) struct FownersCommand {
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for FownersCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.args.len() < 2 {
            return Ok(raise_die(&context, "fowners: usage: fowners owner file..."));
        }
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let owner = self.args[0].clone();
        let files = self.args[1..].to_vec();
        Ok(run_blocking(&context, move || {
            for f in &files {
                let path = under_ed(&ed, f);
                let st = Command::new("chown")
                    .arg(&owner)
                    .arg(&path)
                    .envs(env.iter().cloned())
                    .status();
                ok_status(st, "chown", &path).map_err(|e| format!("fowners: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}
