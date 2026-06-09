use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};

use gentoo_core::Arch;
use portage_atom::Dep;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_metadata::Eapi;

use super::util;
use crate::error::{Error, Result};

/// Stability status of a profile.
///
/// PMS allows repositories to define arbitrary status values beyond the
/// well-known `stable`, `dev`, and `exp`.
///
/// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileStatus {
    /// Stable profile.
    Stable,
    /// Development profile.
    Dev,
    /// Experimental profile.
    Exp,
    /// A repository-defined status value not covered by the well-known variants.
    Other(String),
}

impl ProfileStatus {
    fn parse(s: &str) -> Self {
        match s {
            "stable" => ProfileStatus::Stable,
            "dev" => ProfileStatus::Dev,
            "exp" => ProfileStatus::Exp,
            other => ProfileStatus::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ProfileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileStatus::Stable => f.write_str("stable"),
            ProfileStatus::Dev => f.write_str("dev"),
            ProfileStatus::Exp => f.write_str("exp"),
            ProfileStatus::Other(s) => f.write_str(s),
        }
    }
}

/// A profile entry from `profiles/profiles.desc`.
///
/// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone)]
pub struct ProfileDesc {
    /// Typed architecture keyword.
    arch: Arch,
    /// Path relative to `profiles/` (e.g. `default/linux/amd64/23.0`).
    path: String,
    /// Stability status.
    status: ProfileStatus,
}

impl ProfileDesc {
    /// Parse a single line from `profiles.desc`.
    ///
    /// Format: `arch path status`
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(Error::InvalidProfile(format!(
                "expected 'arch path status', got: {line}"
            )));
        }
        Ok(ProfileDesc {
            arch: Arch::intern(parts[0]),
            path: parts[1].to_string(),
            status: ProfileStatus::parse(parts[2]),
        })
    }

    /// Typed architecture keyword.
    pub fn arch(&self) -> &Arch {
        &self.arch
    }

    /// Path relative to `profiles/` (e.g. `default/linux/amd64/23.0`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Stability status.
    pub fn status(&self) -> &ProfileStatus {
        &self.status
    }
}

/// A profile directory.
///
/// Profiles contain stacked configuration files that control default
/// USE flags, package masking, keywords, and more.
///
/// See [PMS 5 — Profiles](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone)]
pub struct Profile {
    path: PathBuf,
    eapi: Eapi,
}

impl Profile {
    /// Open a profile at the given directory path.
    pub fn open(path: PathBuf) -> Result<Self> {
        let eapi_str = util::read_single_line(path.join("eapi"))?;
        let eapi = match eapi_str {
            Some(s) => s
                .parse::<Eapi>()
                .map_err(|e| Error::InvalidProfile(format!("bad EAPI: {e}")))?,
            None => Eapi::Zero,
        };
        Ok(Profile { path, eapi })
    }

    /// Absolute path to the profile directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The EAPI declared by this profile (from the `eapi` file).
    pub fn eapi(&self) -> Eapi {
        self.eapi
    }

    /// Parse the `parent` file to get parent profile paths.
    ///
    /// Paths are relative to this profile directory and resolved to absolute paths.
    pub fn parents(&self) -> Result<Vec<PathBuf>> {
        let lines = util::read_lines(self.path.join("parent"))?;
        Ok(lines.iter().map(|l| self.path.join(l)).collect())
    }

    /// Parse the `packages` file.
    ///
    /// Returns `(is_system, dep)` pairs. Lines prefixed with `*` indicate
    /// system packages.
    ///
    /// See [PMS 5.2.6](https://projects.gentoo.org/pms/9/pms.html#packages).
    pub fn packages(&self) -> Result<Vec<(bool, Dep)>> {
        let lines = util::read_lines(self.path.join("packages"))?;
        let mut result = Vec::new();
        for line in lines {
            let (is_system, atom_str) = if let Some(rest) = line.strip_prefix('*') {
                (true, rest.trim())
            } else {
                (false, line.as_str())
            };
            let dep = Dep::parse(atom_str)?;
            result.push((is_system, dep));
        }
        Ok(result)
    }

    /// Parse `package.mask`.
    ///
    /// Lines prefixed with `-` remove a previously masked atom (PMS 5.2.8
    /// incremental semantics). Since this is a single-profile view, removals
    /// simply aren't included in the output.
    ///
    /// See [PMS 5.2.8](https://projects.gentoo.org/pms/9/pms.html#packagemask).
    pub fn package_mask(&self) -> Result<Vec<Dep>> {
        let lines = util::read_lines(self.path.join("package.mask"))?;
        let mut result = Vec::new();
        for line in lines {
            if let Some(stripped) = line.strip_prefix('-') {
                let dep = Dep::parse(stripped.trim())?;
                result.retain(|d| d != &dep);
            } else {
                result.push(Dep::parse(line.trim())?);
            }
        }
        Ok(result)
    }

    /// Parse `package.use`.
    ///
    /// Returns `(dep, [flags...])` pairs.
    pub fn package_use(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use"))
    }

    /// Parse `use.force`.
    pub fn use_force(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.force"))
    }

    /// Parse `use.mask`.
    pub fn use_mask(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.mask"))
    }

    /// Parse `use.stable.force`.
    pub fn use_stable_force(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.stable.force"))
    }

    /// Parse `use.stable.mask`.
    pub fn use_stable_mask(&self) -> Result<Vec<String>> {
        util::read_lines(self.path.join("use.stable.mask"))
    }

    /// Parse `package.use.force`.
    pub fn package_use_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.force"))
    }

    /// Parse `package.use.mask`.
    pub fn package_use_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.mask"))
    }

    /// Parse `package.use.stable.force`.
    pub fn package_use_stable_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.stable.force"))
    }

    /// Parse `package.use.stable.mask`.
    pub fn package_use_stable_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.stable.mask"))
    }
}

// ---------------------------------------------------------------------------
// ProfileStack
// ---------------------------------------------------------------------------

/// A fully-resolved profile stack produced by following all `parent` files
/// recursively.
///
/// Profiles are stored in resolution order: root ancestors first, the active
/// (leaf) profile last.  The `use_*` and `package_mask` methods merge entries
/// across the full stack and apply incremental removal (lines prefixed with
/// `-`) per PMS 5.2.5.
///
/// See [PMS 5.1](https://projects.gentoo.org/pms/9/pms.html#profiles) and
/// [PMS 5.2.5](https://projects.gentoo.org/pms/9/pms.html#profile-inheritance).
#[derive(Debug, Clone)]
pub struct ProfileStack {
    /// Profiles in resolution order: root ancestors first, leaf last.
    profiles: Vec<Profile>,
}

impl ProfileStack {
    /// Build the full profile stack for the directory at `path`.
    ///
    /// Follows `parent` files recursively (depth-first).  Each unique profile
    /// directory is included at most once even in diamond-shaped inheritance.
    /// Cycle detection uses canonicalized paths.
    pub fn build(path: PathBuf) -> Result<Self> {
        let mut visited = HashSet::new();
        let profiles = collect_stack(path, &mut visited)?;
        if profiles.is_empty() {
            return Err(Error::InvalidProfile("empty profile stack".into()));
        }
        Ok(ProfileStack { profiles })
    }

    /// Append a site-local user-configuration profile (Portage's
    /// `/etc/portage/profile`) as the final, highest-priority layer.
    ///
    /// Portage treats this directory as a profile appended *after* the resolved
    /// `make.profile` chain, so its `use.force`/`use.mask`/`package.use*`/
    /// `package.mask`/`packages`/`make.defaults` override everything below it
    /// (see portage(5), `LocationsManager`'s `CUSTOM_PROFILE_PATH`). Unlike a
    /// normal profile it is a **flat** node: its own `parent` file is *not*
    /// followed. A no-op when `dir` does not exist or is not a directory.
    pub fn with_user_profile(mut self, dir: PathBuf) -> Result<Self> {
        if dir.is_dir() {
            self.profiles.push(Profile::open(dir)?);
        }
        Ok(self)
    }

    /// All profiles in resolution order: root ancestors first, leaf last.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// The active (leaf) profile — last in the stack.
    pub fn leaf(&self) -> &Profile {
        self.profiles.last().expect("stack is never empty")
    }

    /// Whether the leaf profile has a `deprecated` file.
    ///
    /// See [PMS 5.2.3](https://projects.gentoo.org/pms/9/pms.html#deprecated).
    pub fn is_deprecated(&self) -> bool {
        self.leaf().path().join("deprecated").exists()
    }

    /// Merged `use.force` across the full stack (incremental, `-` removes).
    ///
    /// See [PMS 5.2.9](https://projects.gentoo.org/pms/9/pms.html#use-flags).
    pub fn use_force(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.force"))),
        )
    }

    /// Merged `use.mask` across the full stack (incremental, `-` removes).
    pub fn use_mask(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.mask"))),
        )
    }

    /// Merged `use.stable.force` across the full stack (incremental, `-` removes).
    pub fn use_stable_force(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.stable.force"))),
        )
    }

    /// Merged `use.stable.mask` across the full stack (incremental, `-` removes).
    pub fn use_stable_mask(&self) -> Result<Vec<String>> {
        merge_use_flags(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("use.stable.mask"))),
        )
    }

    /// Merged `package.mask` across the full stack (incremental, `-atom` unmasks).
    ///
    /// See [PMS 5.2.8](https://projects.gentoo.org/pms/9/pms.html#package-mask).
    pub fn package_mask(&self) -> Result<Vec<Dep>> {
        merge_atom_list(
            self.profiles
                .iter()
                .map(|p| read_profile_file(&p.path().join("package.mask"))),
        )
    }

    /// Accumulated `packages` entries across the full stack.
    ///
    /// Returns `(is_system, dep)` pairs from all profiles, ancestors first.
    /// See [PMS 5.2.6](https://projects.gentoo.org/pms/9/pms.html#packages).
    pub fn packages(&self) -> Result<Vec<(bool, Dep)>> {
        let mut acc = Vec::new();
        for p in &self.profiles {
            acc.extend(p.packages()?);
        }
        Ok(acc)
    }

    /// Accumulated `package.use` entries from the full stack, ancestors first.
    ///
    /// Entries should be applied in order; a later entry for the same atom
    /// takes precedence.
    pub fn package_use(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use()))
    }

    /// Accumulated `package.use.force` entries from the full stack.
    pub fn package_use_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_force()))
    }

    /// Accumulated `package.use.mask` entries from the full stack.
    pub fn package_use_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_mask()))
    }

    /// Accumulated `package.use.stable.force` entries from the full stack.
    pub fn package_use_stable_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_stable_force()))
    }

    /// Accumulated `package.use.stable.mask` entries from the full stack.
    pub fn package_use_stable_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        collect_atom_flags(self.profiles.iter().map(|p| p.package_use_stable_mask()))
    }
}

// ---------------------------------------------------------------------------
// UseFlags
// ---------------------------------------------------------------------------

/// The effective resolved USE flags for a package build environment.
///
/// Contains only enabled flags — USE_EXPAND expansions applied,
/// `use.force` added, `use.mask` removed, environment layer applied.
///
/// Produced by [`ProfileStack::use_flags`] (defined in
/// `portage_repo::build::profile`, which has access to [`EbuildShell`]).
#[derive(Debug, Clone, Default)]
pub struct UseFlags(pub(crate) Vec<Interned<DefaultInterner>>);

impl UseFlags {
    /// Iterate over the enabled flag names.
    pub fn iter(&self) -> impl Iterator<Item = &Interned<DefaultInterner>> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl IntoIterator for UseFlags {
    type Item = Interned<DefaultInterner>;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// ---------------------------------------------------------------------------
// ProfileEnvLayer / ProfileEnv
// ---------------------------------------------------------------------------

/// Variables captured from a single `make.defaults` file, evaluated through
/// a real bash shell.
///
/// Each profile level in the stack contributes one layer.  Keeping layers
/// separate allows portage-style incremental accumulation (each level's
/// `USE=` is a delta, not a bash replacement) and makes it easy to trace
/// which profile introduced or removed a particular flag.
///
/// Layers are produced by [`ProfileStack::profile_env`] (defined in
/// `portage_repo::build::profile`, which has access to [`EbuildShell`]).
#[derive(Debug, Clone)]
pub struct ProfileEnvLayer {
    /// Absolute path to the `make.defaults` file this layer was read from.
    pub path: PathBuf,
    /// Variables contributed by this file.
    /// Each value is the raw string as seen by the shell after the file ran;
    /// cross-layer accumulation is handled by [`ProfileEnv::merge`].
    pub(crate) vars: HashMap<String, String>,
}

impl ProfileEnvLayer {
    /// Get the value of a variable from this layer.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// All variable names set in this layer.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.vars.keys().map(String::as_str)
    }
}

/// The stacked profile environment across all layers of a [`ProfileStack`].
///
/// Each [`ProfileEnvLayer`] corresponds to one `make.defaults` file.
/// Variables are not yet merged; call [`merge`](Self::merge) or the
/// higher-level helpers to collapse them with portage-style incremental
/// semantics.
#[derive(Debug, Clone, Default)]
pub struct ProfileEnv {
    /// Layers in resolution order: root ancestors first, leaf last.
    pub layers: Vec<ProfileEnvLayer>,
}

impl ProfileEnv {
    /// Merge a space-separated list variable across all layers using
    /// portage-style incremental semantics.
    ///
    /// Each layer's value is split on whitespace; tokens prefixed with `-`
    /// remove previously accumulated tokens.
    pub fn merge(&self, var: &str) -> Vec<String> {
        merge_flag_lists(self.layers.iter().filter_map(|l| l.get(var)))
    }

    /// The `USE_EXPAND` variable names after merging all layers.
    pub fn use_expand_keys(&self) -> Vec<String> {
        self.merge("USE_EXPAND")
    }

    /// All effective USE flags, including values from `USE_EXPAND` variables
    /// expanded as `lowercase_key_value` tokens, and `USE_EXPAND_UNPREFIXED`
    /// variables expanded directly.
    ///
    /// This is the final profile-level USE; `make.conf` and `use.force`/`use.mask`
    /// have not yet been applied.
    pub fn use_flags(&self) -> Vec<String> {
        let mut flags = self.merge("USE");

        for key in self.merge("USE_EXPAND_UNPREFIXED") {
            for v in self.merge(&key) {
                if !flags.contains(&v) {
                    flags.push(v);
                }
            }
        }

        for key in self.use_expand_keys() {
            let prefix = key.to_lowercase();
            for v in self.merge(&key) {
                let flag = format!("{prefix}_{v}");
                if !flags.contains(&flag) {
                    flags.push(flag);
                }
            }
        }

        flags
    }
}

/// Merge space-separated flag lists with incremental semantics.
///
/// Tokens prefixed with `-` remove previously accumulated tokens.
pub(crate) fn merge_flag_lists<'a>(iter: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut acc: Vec<String> = Vec::new();
    for val in iter {
        for token in val.split_whitespace() {
            if let Some(name) = token.strip_prefix('-') {
                if seen.remove(name) {
                    acc.retain(|f| f != name);
                }
            } else if seen.insert(token.to_string()) {
                acc.push(token.to_string());
            }
        }
    }
    acc
}

/// Recursively collect profiles depth-first, ancestors before self.
///
/// `visited` is a set of canonicalized paths already added; a profile seen a
/// second time (diamond inheritance or cycle) is silently skipped.
fn collect_stack(path: PathBuf, visited: &mut HashSet<PathBuf>) -> Result<Vec<Profile>> {
    let canonical = path.canonicalize().map_err(|e| Error::Io {
        path: path.clone(),
        source: e,
    })?;
    if !visited.insert(canonical.clone()) {
        return Ok(vec![]);
    }
    let profile = Profile::open(canonical)?;
    let mut result = Vec::new();
    for parent in profile.parents()? {
        result.extend(collect_stack(parent, visited)?);
    }
    result.push(profile);
    Ok(result)
}

/// Read non-blank, non-comment lines from a profile file or directory.
///
/// If `path` is a directory (profile-file-dirs, PMS 5.2.5), all regular
/// files inside are read in sorted order and their lines concatenated.
/// Returns an empty `Vec` if `path` does not exist.
fn read_profile_file(path: &Path) -> Result<Vec<String>> {
    if path.is_dir() {
        let mut children: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .collect();
        children.sort();
        let mut lines = Vec::new();
        for child in children {
            if child.is_file() {
                lines.extend(util::read_lines(&child)?);
            }
        }
        Ok(lines)
    } else {
        util::read_lines(path)
    }
}

/// Merge incremental USE-flag lists from a sequence of profile-file results.
///
/// A flag prefixed with `-` removes any previously accumulated occurrence.
fn merge_use_flags<I>(iter: I) -> Result<Vec<String>>
where
    I: Iterator<Item = Result<Vec<String>>>,
{
    let mut seen = std::collections::HashSet::new();
    let mut acc: Vec<String> = Vec::new();
    for chunk in iter {
        for flag in chunk? {
            if let Some(name) = flag.strip_prefix('-') {
                if seen.remove(name) {
                    acc.retain(|f| f != name);
                }
            } else if seen.insert(flag.clone()) {
                acc.push(flag);
            }
        }
    }
    Ok(acc)
}

/// Merge incremental atom lists (`package.mask` format).
///
/// A line prefixed with `-` causes that atom to be removed from the
/// accumulated set.
fn merge_atom_list<I>(iter: I) -> Result<Vec<Dep>>
where
    I: Iterator<Item = Result<Vec<String>>>,
{
    let mut acc: Vec<Dep> = Vec::new();
    for chunk in iter {
        for line in chunk? {
            if let Some(stripped) = line.strip_prefix('-') {
                let dep = Dep::parse(stripped.trim())?;
                acc.retain(|d| d != &dep);
            } else {
                let dep = Dep::parse(line.trim())?;
                if !acc.contains(&dep) {
                    acc.push(dep);
                }
            }
        }
    }
    Ok(acc)
}

/// Concatenate `(atom, flags)` lists from a sequence of profiles.
fn collect_atom_flags<I>(iter: I) -> Result<Vec<(Dep, Vec<String>)>>
where
    I: Iterator<Item = Result<Vec<(Dep, Vec<String>)>>>,
{
    let mut acc = Vec::new();
    for chunk in iter {
        acc.extend(chunk?);
    }
    Ok(acc)
}

// ---------------------------------------------------------------------------
// Existing per-profile helpers
// ---------------------------------------------------------------------------

/// Parse a file containing one dependency atom per line.
/// Parse a file containing `atom flag1 flag2 ...` per line.
fn parse_atom_flags_list(path: &Path) -> Result<Vec<(Dep, Vec<String>)>> {
    let lines = util::read_lines(path)?;
    let mut result = Vec::new();
    for line in lines {
        let mut parts = line.split_whitespace();
        if let Some(atom_str) = parts.next() {
            let dep = Dep::parse(atom_str)?;
            let flags: Vec<String> = parts.map(String::from).collect();
            result.push((dep, flags));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_desc_line() {
        let desc = ProfileDesc::parse("amd64 default/linux/amd64/23.0 stable").unwrap();
        assert_eq!(desc.arch(), "amd64");
        assert_eq!(desc.path(), "default/linux/amd64/23.0");
        assert_eq!(desc.status(), &ProfileStatus::Stable);
    }

    #[test]
    fn parse_profile_desc_dev() {
        let desc = ProfileDesc::parse("arm64 default/linux/arm64/23.0 dev").unwrap();
        assert_eq!(desc.status(), &ProfileStatus::Dev);
    }

    #[test]
    fn parse_profile_desc_exp() {
        let desc = ProfileDesc::parse("riscv default/linux/riscv/23.0 exp").unwrap();
        assert_eq!(desc.status(), &ProfileStatus::Exp);
    }

    #[test]
    fn parse_profile_desc_other_status() {
        let desc = ProfileDesc::parse("x86 some/path testing").unwrap();
        assert_eq!(desc.status(), &ProfileStatus::Other("testing".to_string()));
    }

    #[test]
    fn parse_profile_desc_too_few_fields() {
        assert!(ProfileDesc::parse("amd64 some/path").is_err());
    }

    // --- ProfileStack tests ---

    use std::io::Write as _;
    use tempfile::TempDir;

    /// Create a minimal profile directory with an optional `eapi` file and
    /// an optional `parent` file listing relative paths.
    fn make_profile(dir: &TempDir, name: &str, parents: &[&str]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        // no eapi file → defaults to EAPI 0
        if !parents.is_empty() {
            let mut f = std::fs::File::create(path.join("parent")).unwrap();
            for p in parents {
                writeln!(f, "{p}").unwrap();
            }
        }
        path
    }

    #[test]
    fn stack_single_profile_no_parents() {
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        let stack = ProfileStack::build(p).unwrap();
        assert_eq!(stack.profiles().len(), 1);
    }

    #[test]
    fn stack_linear_chain() {
        // grand → parent → leaf
        let dir = tempfile::tempdir().unwrap();
        make_profile(&dir, "grand", &[]);
        make_profile(&dir, "parent", &["../grand"]);
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        let stack = ProfileStack::build(leaf).unwrap();
        assert_eq!(stack.profiles().len(), 3);
        // leaf must be last
        assert_eq!(stack.leaf().path(), stack.profiles().last().unwrap().path());
    }

    #[test]
    fn stack_diamond_inheritance() {
        // base ← left ← leaf
        //      ← right ←/
        // base must appear exactly once.
        let dir = tempfile::tempdir().unwrap();
        make_profile(&dir, "base", &[]);
        make_profile(&dir, "left", &["../base"]);
        make_profile(&dir, "right", &["../base"]);
        let leaf = make_profile(&dir, "leaf", &["../left", "../right"]);
        let stack = ProfileStack::build(leaf).unwrap();
        assert_eq!(stack.profiles().len(), 4, "base should appear only once");
    }

    #[test]
    fn use_mask_merges_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        // parent masks foo and bar
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("use.mask"), "foo\nbar\n").unwrap();
        // leaf unmasks foo, adds baz
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("use.mask"), "-foo\nbaz\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let masked = stack.use_mask().unwrap();
        assert!(
            !masked.contains(&"foo".to_string()),
            "foo should be unmasked"
        );
        assert!(masked.contains(&"bar".to_string()));
        assert!(masked.contains(&"baz".to_string()));
    }

    #[test]
    fn use_force_merges_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("use.force"), "ipv6\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("use.force"), "-ipv6\nnls\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let forced = stack.use_force().unwrap();
        assert!(!forced.contains(&"ipv6".to_string()));
        assert!(forced.contains(&"nls".to_string()));
    }

    #[test]
    fn user_profile_is_top_layer_and_overrides() {
        // Portage appends /etc/portage/profile as the highest-priority layer.
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);
        std::fs::write(leaf.join("use.force"), "ipv6\n").unwrap();

        let user = dir.path().join("user-profile");
        std::fs::create_dir(&user).unwrap();
        // Unforce ipv6 (incremental removal from the top) and force nls.
        std::fs::write(user.join("use.force"), "-ipv6\nnls\n").unwrap();

        let stack = ProfileStack::build(leaf)
            .unwrap()
            .with_user_profile(user)
            .unwrap();
        let forced = stack.use_force().unwrap();
        assert!(!forced.contains(&"ipv6".to_string()), "user layer unforced ipv6");
        assert!(forced.contains(&"nls".to_string()), "user layer forced nls");
    }

    #[test]
    fn user_profile_reads_directory_form_package_use_mask() {
        // /etc/portage/profile/package.use.mask is commonly a directory.
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);

        let user = dir.path().join("user-profile");
        std::fs::create_dir(&user).unwrap();
        let pum = user.join("package.use.mask");
        std::fs::create_dir(&pum).unwrap();
        std::fs::write(pum.join("cross"), "cross-foo/gcc multilib cet\n").unwrap();

        let stack = ProfileStack::build(leaf)
            .unwrap()
            .with_user_profile(user)
            .unwrap();
        let masked = stack.package_use_mask().unwrap();
        let entry = masked
            .iter()
            .find(|(d, _)| d.to_string().contains("cross-foo/gcc"))
            .expect("directory-form package.use.mask read");
        assert!(entry.1.contains(&"multilib".to_string()));
        assert!(entry.1.contains(&"cet".to_string()));
    }

    #[test]
    fn with_user_profile_absent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let leaf = make_profile(&dir, "leaf", &[]);
        let before = ProfileStack::build(leaf.clone()).unwrap().profiles().len();
        let after = ProfileStack::build(leaf)
            .unwrap()
            .with_user_profile(dir.path().join("does-not-exist"))
            .unwrap()
            .profiles()
            .len();
        assert_eq!(before, after, "absent user profile must not add a layer");
    }

    #[test]
    fn package_mask_merges_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("package.mask"), "dev-libs/foo\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("package.mask"), "-dev-libs/foo\ndev-libs/bar\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let masked = stack.package_mask().unwrap();
        let names: Vec<_> = masked.iter().map(|d| d.to_string()).collect();
        assert!(!names.iter().any(|n| n.contains("foo")), "foo unmasked");
        assert!(names.iter().any(|n| n.contains("bar")));
    }

    #[test]
    fn is_deprecated_false_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        let stack = ProfileStack::build(p).unwrap();
        assert!(!stack.is_deprecated());
    }

    #[test]
    fn is_deprecated_true_when_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        std::fs::write(p.join("deprecated"), "Use foo instead.\n").unwrap();
        let stack = ProfileStack::build(p).unwrap();
        assert!(stack.is_deprecated());
    }

    #[test]
    fn packages_accumulated_from_stack() {
        let dir = tempfile::tempdir().unwrap();
        let parent = make_profile(&dir, "parent", &[]);
        std::fs::write(parent.join("packages"), "*sys-libs/glibc\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../parent"]);
        std::fs::write(leaf.join("packages"), "*sys-kernel/linux-headers\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let pkgs = stack.packages().unwrap();
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().all(|(is_sys, _)| *is_sys));
    }

    #[test]
    fn directory_as_file_use_mask() {
        // profile-file-dirs: use.mask is a directory with multiple files
        let dir = tempfile::tempdir().unwrap();
        let p = make_profile(&dir, "leaf", &[]);
        let mask_dir = p.join("use.mask");
        std::fs::create_dir(&mask_dir).unwrap();
        std::fs::write(mask_dir.join("01-base"), "foo\nbar\n").unwrap();
        std::fs::write(mask_dir.join("02-extra"), "baz\n").unwrap();

        let stack = ProfileStack::build(p).unwrap();
        let masked = stack.use_mask().unwrap();
        assert_eq!(masked, vec!["foo", "bar", "baz"]);
    }
}
