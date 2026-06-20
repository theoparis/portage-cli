//! Shell-dependent profile operations.
//!
//! Methods that source bash files (make.defaults) or read shell state (USE
//! flags) cannot live in `repo/profile.rs` without creating an upward
//! dependency from the repo module into the build module.  They live here
//! instead and are added to `Profile` / `ProfileStack` via second impl blocks.

use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};

use super::shell::EbuildShell;
use crate::error::Result;
use crate::repo::profile::{
    Profile, ProfileEnv, ProfileEnvLayer, ProfileStack, UseFlags, merge_flag_lists,
};

impl Profile {
    /// Source this profile's `make.defaults` into the shell, if present.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#makedefaults).
    pub async fn make_defaults(&self, shell: &mut EbuildShell) -> Result<()> {
        let path = self.path().join("make.defaults");
        if path.is_file() {
            shell.source_make_defaults(&path).await?;
        }
        Ok(())
    }
}

impl ProfileStack {
    /// Source `make.defaults` from every profile in stack order.
    ///
    /// Variables set by ancestor profiles are visible to child profiles.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#make-defaults).
    pub async fn make_defaults(&self, shell: &mut EbuildShell) -> Result<()> {
        for p in self.profiles() {
            p.make_defaults(shell).await?;
        }
        Ok(())
    }

    /// Resolve the effective USE flags for this profile stack.
    ///
    /// Sources profile `make.defaults`, extra confs (e.g. `/etc/portage/make.conf`),
    /// then applies the process-environment layer, `use.force`, and `use.mask`.
    ///
    /// Returns enabled-only flags as interned strings.  The shell's bash state is
    /// updated as a side-effect (necessary for bash evaluation); the Rust-side
    /// `use_flags` HashSet is **not** set — call `configure_shell` for that.
    pub async fn use_flags(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[&std::path::Path],
    ) -> Result<UseFlags> {
        resolve_use_flags(shell, self, extra_confs).await
    }

    /// Build the layered profile environment by sourcing each `make.defaults`
    /// through brush with per-layer isolation.
    ///
    /// Each file is sourced in the **same** shell (preserving cross-file
    /// variable visibility for non-incremental vars like `EAPI`, computed
    /// paths, etc.).  Before each file the incremental variables (`USE`,
    /// `USE_EXPAND`, and every key listed in `USE_EXPAND`) are reset to
    /// empty so the file's own assignments are captured as a clean delta.
    /// After sourcing, the accumulated values are restored into the shell
    /// for the next file to reference via `${USE}`, `${PYTHON_TARGETS}`, etc.
    ///
    /// When this method returns the shell holds the fully-accumulated profile
    /// state, ready for `make.conf` to be sourced on top.
    pub async fn profile_env(&self, shell: &mut EbuildShell) -> Result<ProfileEnv> {
        let mut layers: Vec<ProfileEnvLayer> = Vec::new();
        // External accumulator: keeps the merged state per incremental var.
        let mut acc: HashMap<String, String> = HashMap::new();

        for profile in self.profiles() {
            let path = profile.path().join("make.defaults");
            if !path.is_file() {
                continue;
            }

            // Determine which vars to isolate for this layer.
            // Start with the fixed incremental vars, then add all currently
            // known USE_EXPAND keys so their values are also captured cleanly.
            let mut incr: Vec<String> = vec![
                "USE".into(),
                "USE_EXPAND".into(),
                "USE_EXPAND_HIDDEN".into(),
                "USE_EXPAND_IMPLICIT".into(),
                "USE_EXPAND_UNPREFIXED".into(),
            ];
            let expand_now = acc.get("USE_EXPAND").cloned().unwrap_or_default();
            for key in expand_now.split_whitespace() {
                if !incr.contains(&key.to_string()) {
                    incr.push(key.to_string());
                }
            }
            let unprefixed_now = acc
                .get("USE_EXPAND_UNPREFIXED")
                .cloned()
                .unwrap_or_default();
            for key in unprefixed_now.split_whitespace() {
                if !incr.contains(&key.to_string()) {
                    incr.push(key.to_string());
                }
            }

            // Reset all incremental vars to empty so this file's assignments
            // are its pure contribution, not a replacement of accumulated state.
            let reset: String = incr.iter().map(|v| format!("{}=\"\"\n", v)).collect();
            shell.run_string(&reset).await?;

            // Source the file through brush — all bash features available,
            // cross-file non-incremental vars (set by earlier files) are visible.
            shell.source_make_defaults(&path).await?;

            // Capture this layer's contributions.
            let mut vars: HashMap<String, String> = HashMap::new();

            // Collect the fixed incremental vars.
            for var in &incr {
                if let Some(val) = shell.get_var(var)
                    && !val.is_empty()
                {
                    vars.insert(var.clone(), val);
                }
            }
            // Discover any new USE_EXPAND keys this file introduced.
            let new_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
            for key in new_expand.split_whitespace() {
                if !incr.contains(&key.to_string())
                    && let Some(val) = shell.get_var(key)
                    && !val.is_empty()
                {
                    vars.insert(key.to_string(), val);
                }
            }
            // Same for USE_EXPAND_UNPREFIXED keys.
            let new_unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();
            for key in new_unprefixed.split_whitespace() {
                if !incr.contains(&key.to_string())
                    && let Some(val) = shell.get_var(key)
                    && !val.is_empty()
                {
                    vars.insert(key.to_string(), val);
                }
            }

            // Merge this layer's contributions into the external accumulator.
            for (var, val) in &vars {
                let prev = acc.get(var.as_str()).cloned().unwrap_or_default();
                let merged = merge_flag_lists([prev.as_str(), val.as_str()].into_iter());
                acc.insert(var.clone(), merged.join(" "));
            }

            // Restore the accumulated state into the shell so the next file
            // sees the full inherited environment.
            let restore: String = acc
                .iter()
                .map(|(k, v)| format!("{}={}\n", k, shell_quote(v)))
                .collect();
            shell.run_string(&restore).await?;

            layers.push(ProfileEnvLayer { path, vars });
        }

        Ok(ProfileEnv { layers })
    }

    /// Configure a shell with this profile stack's effective USE flags.
    ///
    /// See `configure_shell` for the full description.
    pub async fn configure_shell(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[&std::path::Path],
    ) -> Result<()> {
        configure_shell(shell, self, extra_confs).await
    }
}

/// Configure a shell with the effective USE flags from a profile stack.
///
/// Calls [`resolve_use_flags`] then sets the Rust-side `use_flags` HashSet so
/// the `use()` / `usev()` / `usex()` builtins work correctly during phase execution.
///
/// See [`resolve_use_flags`] for the full evaluation order.
pub async fn configure_shell(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[&std::path::Path],
) -> Result<()> {
    let flags = resolve_use_flags(shell, stack, extra_confs).await?;
    let refs: Vec<&str> = flags.iter().map(|f| f.as_str()).collect();
    shell.set_use_flags(&refs)
}

/// Resolve the effective USE flags for a profile stack without setting the
/// Rust-side execution state.
///
/// `extra_confs` is a list of additional shell scripts (e.g.
/// `/etc/portage/make.conf`) sourced **after** the profile env is applied but
/// **before** `use.force`/`use.mask` are applied.
///
/// Computation order:
/// 1. Each `make.defaults` sourced through brush with per-layer USE isolation
///    (see [`ProfileStack::profile_env`])
/// 2. Each `extra_confs` script sourced with the same incremental treatment
/// 3. Process-environment layer: `USE`, `USE_EXPAND` keys, and
///    `USE_EXPAND_UNPREFIXED` keys read from `std::env` and merged
/// 4. `USE_EXPAND_UNPREFIXED` values expanded directly into USE
/// 5. `USE_EXPAND` values expanded with lowercase prefix
/// 6. Profile `use.force` — unconditional add
/// 7. Profile `use.mask` — unconditional remove
///
/// The shell's bash state is updated as a side-effect.
async fn resolve_use_flags(
    shell: &mut EbuildShell,
    stack: &ProfileStack,
    extra_confs: &[&std::path::Path],
) -> Result<UseFlags> {
    let ProfileEnv { layers: _ } = stack.profile_env(shell).await?;

    for conf in extra_confs {
        source_incremental(shell, conf).await?;
    }

    apply_env_layer(shell).await?;

    let mut flags = collect_use_flags(shell);

    let flag_set: HashSet<String> = flags.iter().cloned().collect();
    for flag in stack.use_force()? {
        if !flag_set.contains(&flag) {
            flags.push(flag);
        }
    }

    let mask: HashSet<String> = stack.use_mask()?.into_iter().collect();
    flags.retain(|f| !mask.contains(f.as_str()));

    Ok(UseFlags(
        flags
            .iter()
            .map(|f| Interned::<DefaultInterner>::intern(f.as_str()))
            .collect(),
    ))
}

/// Merge process-environment USE variables into the shell as a final incremental layer.
///
/// Reads `USE`, all `USE_EXPAND` keys, and all `USE_EXPAND_UNPREFIXED` keys from
/// `std::env`.  Any present values are merged with the accumulated shell state using
/// the same incremental semantics as profile layers (tokens prefixed with `-` remove).
///
/// This is how `PYTHON_TARGETS=python3_15 em cat/pkg` adds a target without
/// replacing the full flag set, mirroring how `CC=my-cc` is applied in `init_build_env`.
async fn apply_env_layer(shell: &mut EbuildShell) -> Result<()> {
    let use_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    let unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();

    let mut vars = vec!["USE".to_string()];
    for k in use_expand.split_whitespace() {
        vars.push(k.to_string());
    }
    for k in unprefixed.split_whitespace() {
        if !vars.contains(&k.to_string()) {
            vars.push(k.to_string());
        }
    }

    let mut restore = String::new();
    for var in &vars {
        if let Ok(env_val) = std::env::var(var) {
            let existing = shell.get_var(var).unwrap_or_default();
            let merged = merge_flag_lists([existing.as_str(), env_val.as_str()].into_iter());
            restore += &format!("{}={}\n", var, shell_quote(&merged.join(" ")));
        }
    }
    if !restore.is_empty() {
        shell.run_string(&restore).await?;
    }
    Ok(())
}

/// Source a single config file (e.g. `make.conf`) with incremental USE semantics.
///
/// Before sourcing, the incremental vars (`USE` and all known `USE_EXPAND`
/// keys) are reset to empty so the file's own assignments represent its pure
/// contribution.  After sourcing, those contributions are merged back into
/// the accumulated shell state using [`merge_flag_lists`].
async fn source_incremental(shell: &mut EbuildShell, path: &std::path::Path) -> Result<()> {
    // Collect the set of incremental vars to isolate.
    let mut incr: Vec<String> = vec![
        "USE".into(),
        "USE_EXPAND".into(),
        "USE_EXPAND_HIDDEN".into(),
        "USE_EXPAND_IMPLICIT".into(),
        "USE_EXPAND_UNPREFIXED".into(),
    ];
    let expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    for key in expand.split_whitespace() {
        if !incr.contains(&key.to_string()) {
            incr.push(key.to_string());
        }
    }
    let unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();
    for key in unprefixed.split_whitespace() {
        if !incr.contains(&key.to_string()) {
            incr.push(key.to_string());
        }
    }

    // Save current accumulated values and reset vars to empty.
    let saved: HashMap<String, String> = incr
        .iter()
        .filter_map(|v| shell.get_var(v).map(|val| (v.clone(), val)))
        .collect();

    let reset: String = incr.iter().map(|v| format!("{}=\"\"\n", v)).collect();
    shell.run_string(&reset).await?;

    // Source the file through brush.
    shell.source_make_defaults(path).await?;

    // Collect this file's contributions.
    let mut contributed: HashMap<String, String> = HashMap::new();
    for var in &incr {
        if let Some(val) = shell.get_var(var)
            && !val.is_empty()
        {
            contributed.insert(var.clone(), val);
        }
    }
    // Pick up any new USE_EXPAND keys the file introduced.
    let new_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    for key in new_expand.split_whitespace() {
        if !incr.contains(&key.to_string())
            && let Some(val) = shell.get_var(key)
            && !val.is_empty()
        {
            contributed.insert(key.to_string(), val);
        }
    }

    // Merge contributions with saved state and restore.
    let mut merged_acc: HashMap<String, String> = saved;
    for (var, new_val) in &contributed {
        let prev = merged_acc.get(var.as_str()).cloned().unwrap_or_default();
        let merged = merge_flag_lists([prev.as_str(), new_val.as_str()].into_iter());
        merged_acc.insert(var.clone(), merged.join(" "));
    }

    let restore: String = merged_acc
        .iter()
        .map(|(k, v)| format!("{}={}\n", k, shell_quote(v)))
        .collect();
    shell.run_string(&restore).await?;

    Ok(())
}

/// Quote a value for use in a bash assignment (`VAR="..."` form).
fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn collect_use_flags(shell: &EbuildShell) -> Vec<String> {
    let use_str = shell.get_var("USE").unwrap_or_default();
    let mut flags: Vec<String> = Vec::new();

    for token in use_str.split_whitespace() {
        if let Some(name) = token.strip_prefix('-') {
            flags.retain(|f| f != name);
        } else if !flags.iter().any(|f| f == token) {
            flags.push(token.to_string());
        }
    }

    let unprefixed = shell.get_var("USE_EXPAND_UNPREFIXED").unwrap_or_default();
    for var in unprefixed.split_whitespace() {
        let val = shell.get_var(var).unwrap_or_default();
        for v in val.split_whitespace() {
            if !flags.iter().any(|f| f == v) {
                flags.push(v.to_string());
            }
        }
    }

    let use_expand = shell.get_var("USE_EXPAND").unwrap_or_default();
    for var in use_expand.split_whitespace() {
        let val = shell.get_var(var).unwrap_or_default();
        let prefix = var.to_lowercase();
        for v in val.split_whitespace() {
            let flag = format!("{prefix}_{v}");
            if !flags.iter().any(|f| f == &flag) {
                flags.push(flag);
            }
        }
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::profile::ProfileStack;
    use crate::repo::repository::Repository;
    use std::collections::HashSet;
    use tempfile::TempDir;

    fn make_profile(dir: &TempDir, name: &str, parents: &[&str]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let parent_content = parents
            .iter()
            .map(|p| format!("{}\n", p))
            .collect::<String>();
        if !parent_content.is_empty() {
            std::fs::write(path.join("parent"), &parent_content).unwrap();
        }
        path
    }

    fn make_test_repo(dir: &TempDir) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        Repository::open(dir.path()).unwrap()
    }

    #[tokio::test]
    async fn source_env_file_composes_features_and_overrides_flags() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        // Baseline make.conf-ish environment.
        std::fs::write(
            profile.join("make.defaults"),
            "FEATURES=\"sandbox\"\nCFLAGS=\"-O2\"\n",
        )
        .unwrap();
        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        // A package.env file sourced on top: FEATURES composes, CFLAGS replaces.
        let env_file = dir.path().join("ccache");
        std::fs::write(
            &env_file,
            "FEATURES=\"${FEATURES} ccache\"\nCFLAGS=\"-O3\"\n",
        )
        .unwrap();
        shell.source_env_file(&env_file).await.unwrap();

        let features_var = shell.get_var("FEATURES").unwrap_or_default();
        let features: HashSet<&str> = features_var.split_whitespace().collect();
        assert!(features.contains("sandbox"), "baseline FEATURES kept");
        assert!(features.contains("ccache"), "env-file FEATURES composed in");
        assert_eq!(
            shell.get_var("CFLAGS").as_deref(),
            Some("-O3"),
            "CFLAGS replaced"
        );
    }

    #[tokio::test]
    async fn configure_shell_applies_make_defaults_use() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"foo bar\"\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("foo"), "foo from make.defaults");
        assert!(flags.contains("bar"), "bar from make.defaults");
    }

    #[tokio::test]
    async fn configure_shell_applies_use_force_and_mask() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"foo bar\"\n").unwrap();
        std::fs::write(profile.join("use.force"), "forced\n").unwrap();
        std::fs::write(profile.join("use.mask"), "bar\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("foo"));
        assert!(!flags.contains("bar"), "bar should be masked");
        assert!(
            flags.contains("forced"),
            "forced should be added by use.force"
        );
    }

    #[tokio::test]
    async fn configure_shell_expands_use_expand() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND=\"CPU_FLAGS_X86\"\nCPU_FLAGS_X86=\"sse2 mmx\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("cpu_flags_x86_sse2"), "sse2 expanded");
        assert!(flags.contains("cpu_flags_x86_mmx"), "mmx expanded");
    }

    #[tokio::test]
    async fn configure_shell_expands_use_expand_unprefixed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(
            profile.join("make.defaults"),
            "USE_EXPAND_UNPREFIXED=\"ARCH\"\nARCH=\"amd64\"\n",
        )
        .unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("amd64"), "ARCH added unprefixed");
    }

    #[tokio::test]
    async fn configure_shell_make_conf_applied_before_force_mask() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let profile = make_profile(&dir, "test", &[]);
        std::fs::write(profile.join("make.defaults"), "USE=\"foo bar forced\"\n").unwrap();
        std::fs::write(profile.join("use.force"), "forced\n").unwrap();
        std::fs::write(profile.join("use.mask"), "bar\n").unwrap();

        let make_conf = dir.path().join("make.conf");
        std::fs::write(&make_conf, "USE=\"${USE} user_flag -forced\"\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[make_conf.as_path()])
            .await
            .unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("foo"));
        assert!(flags.contains("user_flag"), "user_flag from make.conf");
        assert!(!flags.contains("bar"), "bar masked by use.mask");
        assert!(
            flags.contains("forced"),
            "use.force overrides make.conf removal"
        );
    }

    /// Two-layer profile: base sets unicode, child sets crypt — both must survive.
    #[tokio::test]
    async fn configure_shell_two_layer_use_accumulation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let base = make_profile(&dir, "base", &[]);
        std::fs::write(base.join("make.defaults"), "USE=\"unicode acl\"\n").unwrap();
        let leaf = make_profile(&dir, "leaf", &["../base"]);
        // Overwrites USE (no ${USE}) — this is the bug pattern we're fixing.
        std::fs::write(leaf.join("make.defaults"), "USE=\"crypt ssl\"\n").unwrap();

        let stack = ProfileStack::build(leaf).unwrap();
        let mut shell = repo.shell().await.unwrap();
        configure_shell(&mut shell, &stack, &[]).await.unwrap();

        let use_val = shell.get_var("USE").unwrap_or_default();
        let flags: HashSet<&str> = use_val.split_whitespace().collect();
        assert!(flags.contains("unicode"), "unicode from base must survive");
        assert!(flags.contains("acl"), "acl from base must survive");
        assert!(flags.contains("crypt"), "crypt from leaf must be present");
        assert!(flags.contains("ssl"), "ssl from leaf must be present");
    }
}
