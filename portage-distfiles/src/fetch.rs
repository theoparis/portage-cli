use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{Manifest, ManifestEntry};
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};
use crate::resolver::Distfile;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Strategy for downloading a distfile.
///
/// `Builtin` uses the embedded reqwest client.  `Command` shells out to an
/// external program using the same template variables as Portage's
/// `FETCHCOMMAND` / `RESUMECOMMAND` make.conf settings.
#[derive(Debug, Clone, Default)]
pub enum FetchStrategy {
    /// Built-in reqwest HTTP client (default).
    #[default]
    Builtin,
    /// External command template.
    ///
    /// Template variables (same as Portage):
    /// - `${URI}` — the full download URL
    /// - `${FILE}` — just the filename
    /// - `${DISTDIR}` — the distfiles directory path
    Command(String),
}

/// Fetch and resume configuration.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Primary fetch strategy.  Defaults to `Builtin`.
    pub strategy: FetchStrategy,
    /// Fallback command template used when the primary strategy fails.
    pub fallback_command: Option<String>,
    /// Resume command template (`RESUMECOMMAND`).
    pub resume_command: Option<String>,
    /// Maximum number of distfiles fetched concurrently.  Defaults to 4.
    pub max_concurrent: usize,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            strategy: FetchStrategy::default(),
            fallback_command: None,
            resume_command: None,
            max_concurrent: 4,
        }
    }
}

impl FetchConfig {
    /// Build from `make.conf`-style environment/config values.
    pub fn from_make_conf(fetch_command: Option<String>, resume_command: Option<String>) -> Self {
        match fetch_command {
            Some(cmd) => Self {
                strategy: FetchStrategy::Command(cmd),
                resume_command,
                ..Self::default()
            },
            None => Self {
                strategy: FetchStrategy::Builtin,
                resume_command,
                ..Self::default()
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------

/// Outcome of a single fetch operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchStatus {
    /// File was already present and passed manifest verification.
    AlreadyPresent,
    /// File was downloaded and verified successfully.
    Downloaded,
    /// RESTRICT=fetch — the distfile must not be auto-fetched.
    ///
    /// The caller should run the ebuild's `pkg_nofetch` phase, which prints
    /// manual download instructions.
    FetchRestricted,
}

/// Downloads and verifies distfiles.
#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    distdir: Utf8PathBuf,
    /// Read-only distfile locations searched before downloading
    /// (`PORTAGE_RO_DISTDIRS` semantics — e.g. the system distdir when the
    /// writable one is a per-user directory).
    ro_distdirs: Vec<Utf8PathBuf>,
    config: FetchConfig,
}

impl Fetcher {
    pub fn new(distdir: Utf8PathBuf, config: FetchConfig) -> Self {
        // Send a User-Agent: some mirrors (e.g. freedesktop.org's Apache)
        // return HTTP 403 for requests with an empty/missing UA, mirroring how
        // portage's default wget/curl FETCHCOMMAND always identifies itself.
        let client = reqwest::Client::builder()
            .user_agent(concat!("em/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            distdir,
            ro_distdirs: Vec::new(),
            config,
        }
    }

    /// Add read-only locations consulted for already-present distfiles.
    pub fn with_ro_distdirs(mut self, dirs: Vec<Utf8PathBuf>) -> Self {
        self.ro_distdirs = dirs;
        self
    }

    /// Fetch a single distfile, verifying it against `manifest`.
    ///
    /// If the file already exists and passes verification it is not
    /// re-downloaded.  If a partial file is present a resume is attempted.
    pub async fn fetch_distfile(&self, df: &Distfile, manifest: &Manifest) -> Result<FetchStatus> {
        // RESTRICT=fetch: the ebuild forbids automatic downloading.
        // Return immediately so the caller can run pkg_nofetch.
        if df.restriction.as_deref() == Some("fetch") {
            return Ok(FetchStatus::FetchRestricted);
        }

        let dest = self.distdir.join(&df.filename);

        let manifest_entry = manifest_entry_for(manifest, &df.filename);

        // Fast path: already present and valid (writable dir first, then the
        // read-only locations).
        for dir in std::iter::once(&self.distdir).chain(self.ro_distdirs.iter()) {
            let candidate = dir.join(&df.filename);
            if !candidate.exists() {
                continue;
            }
            let valid = match manifest_entry {
                Some(entry) => entry.verify_file(candidate.as_std_path()).is_ok(),
                // No manifest entry to verify against — treat as present.
                None => candidate.is_file(),
            };
            if !valid {
                continue;
            }
            // Found in a read-only distdir, not the writable DISTDIR: expose it
            // in DISTDIR (portage symlinks RO distfiles in) so unpack/eapply —
            // which only look in DISTDIR — find it. Without this, em reports
            // "already present" for a file the build then can't open (e.g.
            // bash's `bash53-NNN` patches under /var/cache/distfiles).
            if dir != &self.distdir {
                link_into_distdir(&candidate, &dest);
            }
            return Ok(FetchStatus::AlreadyPresent);
        }

        if df.urls.is_empty() {
            return Err(Error::AllFailed {
                filename: df.filename.clone(),
            });
        }

        // Try each URL in order.
        let mut last_err = None;
        for url in &df.urls {
            let result = self.fetch_one_url(url, &dest, manifest_entry).await;
            match result {
                Ok(()) => return Ok(FetchStatus::Downloaded),
                Err(e) => {
                    eprintln!("fetch: {url}: {e}");
                    last_err = Some(e);
                }
            }
        }

        // Primary strategy exhausted — try fallback command if configured.
        if let Some(cmd_template) = &self.config.fallback_command {
            for url in &df.urls {
                let result = self
                    .run_command(cmd_template, url, &df.filename, &dest)
                    .await;
                if result.is_ok() {
                    verify_or_discard(manifest_entry, &dest)?;
                    return Ok(FetchStatus::Downloaded);
                }
                last_err = result.err();
            }
        }

        Err(last_err.unwrap_or(Error::AllFailed {
            filename: df.filename.clone(),
        }))
    }

    /// Fetch all distfiles in parallel, returning per-file results in input order.
    ///
    /// Up to `config.max_concurrent` downloads run simultaneously.
    /// Each result is paired with the originating [`Distfile`] reference.
    pub async fn fetch_all<'a>(
        &self,
        distfiles: &'a [Distfile],
        manifest: &Manifest,
    ) -> Vec<(&'a Distfile, Result<FetchStatus>)> {
        use futures_util::StreamExt;
        use std::sync::Arc;

        let fetcher = Arc::new(self.clone());
        let manifest = Arc::new(manifest.clone());
        let max = self.config.max_concurrent.max(1);

        futures_util::stream::iter(distfiles)
            .map(|df| {
                let fetcher = Arc::clone(&fetcher);
                let manifest = Arc::clone(&manifest);
                async move {
                    let r = fetcher.fetch_distfile(df, &manifest).await;
                    (df, r)
                }
            })
            .buffer_unordered(max)
            .collect()
            .await
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    async fn fetch_one_url(
        &self,
        url: &str,
        dest: &Utf8Path,
        manifest_entry: Option<&ManifestEntry>,
    ) -> Result<()> {
        match &self.config.strategy {
            FetchStrategy::Builtin => self.fetch_builtin(url, dest, manifest_entry).await,
            FetchStrategy::Command(template) => {
                self.run_command(template, url, dest.file_name().unwrap_or(""), dest)
                    .await?;
                verify_or_discard(manifest_entry, dest)
            }
        }
    }

    async fn fetch_builtin(
        &self,
        url: &str,
        dest: &Utf8Path,
        manifest_entry: Option<&ManifestEntry>,
    ) -> Result<()> {
        let expected_size = manifest_entry.and_then(dist_size);
        let existing_size = current_size(dest);

        // Try to resume a *plausible* partial first (cheap when it is a genuine
        // prefix). If the resume produces a verified file we are done; otherwise
        // we fall through. The resume is never trusted on its own.
        if is_resumable(expected_size, existing_size)
            && self
                .resume_partial(url, dest, existing_size, manifest_entry)
                .await?
        {
            return Ok(());
        }

        // Either there was nothing worth resuming, or the resume did not yield a
        // valid file. Discard whatever is on disk and download the whole file
        // fresh — a corrupt/short/HTML leftover must never linger to be Ranged
        // into on the next URL or run (the psmisc-class failure).
        let _ = std::fs::remove_file(dest.as_std_path());
        self.download_full(url, dest, manifest_entry).await
    }

    /// Resume a partial via `RESUMECOMMAND` (if set) or an HTTP `Range` request.
    /// Returns `Ok(true)` only when the resumed file verifies against the
    /// manifest; `Ok(false)` means "couldn't resume — download fresh instead".
    async fn resume_partial(
        &self,
        url: &str,
        dest: &Utf8Path,
        existing_size: u64,
        manifest_entry: Option<&ManifestEntry>,
    ) -> Result<bool> {
        if let Some(resume_tmpl) = &self.config.resume_command {
            let ran = self
                .run_command(resume_tmpl, url, dest.file_name().unwrap_or(""), dest)
                .await
                .is_ok();
            return Ok(ran && verify_ok(manifest_entry, dest));
        }

        let response = self
            .client
            .get(url)
            .header("Range", format!("bytes={existing_size}-"))
            .send()
            .await
            .map_err(|e| Error::Network {
                url: url.to_owned(),
                source: e,
            })?;

        // Only a 206 actually continues the partial; a 200 means the server
        // ignored Range and is resending from byte 0 — let the fresh path own
        // that (it truncates), and reject an HTML error/redirect body outright.
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT || is_html(&response) {
            return Ok(false);
        }
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(dest.as_std_path())
            .await
            .map_err(|e| Error::Io {
                path: dest.to_path_buf().into_std_path_buf(),
                source: e,
            })?;
        stream_to_file(url, response, &mut file, dest).await?;
        Ok(verify_ok(manifest_entry, dest))
    }

    /// Download the entire file fresh (no `Range`), rejecting obvious non-file
    /// bodies and verifying against the manifest. A body that fails verification
    /// is removed so it can't masquerade as a resumable partial next time.
    async fn download_full(
        &self,
        url: &str,
        dest: &Utf8Path,
        manifest_entry: Option<&ManifestEntry>,
    ) -> Result<()> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Network {
                url: url.to_owned(),
                source: e,
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Http {
                url: url.to_owned(),
                status: status.as_u16(),
            });
        }
        // A distfile is never HTML; a 2xx `text/html` body is an error/redirect
        // page (e.g. a SourceForge "file not found"/mirror picker), not the
        // archive — caching it would fail verification on every retry forever.
        if is_html(&response) {
            return Err(Error::Verify {
                filename: dest.file_name().unwrap_or("?").to_owned(),
                reason: "server returned an HTML body, not the distfile".to_owned(),
            });
        }

        let mut file = tokio::fs::File::create(dest.as_std_path())
            .await
            .map_err(|e| Error::Io {
                path: dest.to_path_buf().into_std_path_buf(),
                source: e,
            })?;
        stream_to_file(url, response, &mut file, dest).await?;
        verify_or_discard(manifest_entry, dest)
    }

    /// Execute a FETCHCOMMAND/RESUMECOMMAND template.
    ///
    /// Template substitution: `${URI}` → url, `${FILE}` → filename,
    /// `${DISTDIR}` → distdir path.  The expanded command is run via `sh -c`.
    async fn run_command(
        &self,
        template: &str,
        url: &str,
        filename: &str,
        _dest: &Utf8Path,
    ) -> Result<()> {
        let cmd = template
            .replace("${URI}", url)
            .replace("${FILE}", filename)
            .replace("${DISTDIR}", self.distdir.as_str());

        let status = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .status()
            .await
            .map_err(|e| Error::CommandSpawn { source: e })?;

        if status.success() {
            Ok(())
        } else {
            Err(Error::Command {
                command: cmd,
                code: status.code().unwrap_or(-1),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn manifest_entry_for<'a>(manifest: &'a Manifest, filename: &str) -> Option<&'a ManifestEntry> {
    manifest.dist_entries().find(|e| {
        if let ManifestEntry::Dist { filename: f, .. } = e {
            f == filename
        } else {
            false
        }
    })
}

/// Current on-disk size of `dest`, or 0 when it is absent or unreadable.
fn current_size(dest: &Utf8Path) -> u64 {
    std::fs::metadata(dest.as_std_path())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// The manifest's recorded size for a distfile entry (`None` for non-`Dist`).
fn dist_size(entry: &ManifestEntry) -> Option<u64> {
    match entry {
        ManifestEntry::Dist { size, .. } => Some(*size),
        _ => None,
    }
}

/// A leftover file is a resumable partial only when its size is a *strict*
/// prefix of the target: present, and smaller than the known manifest size.
/// Without a known size we never resume (a blind `Range` onto an unknown body is
/// how a corrupt cache wedges every retry); a complete-but-wrong file (`>=`
/// expected) is refetched fresh, not appended to.
fn is_resumable(expected_size: Option<u64>, existing_size: u64) -> bool {
    matches!(expected_size, Some(exp) if existing_size > 0 && existing_size < exp)
}

/// Whether a response carries an HTML body — never a distfile, so it's an
/// error/redirect page to be rejected rather than saved.
fn is_html(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            ct.trim_start()
                .to_ascii_lowercase()
                .starts_with("text/html")
        })
        .unwrap_or(false)
}

/// Verify `dest` against the manifest if there is an entry; `true` when it passes
/// (or there is nothing to check against).
fn verify_ok(manifest_entry: Option<&ManifestEntry>, dest: &Utf8Path) -> bool {
    match manifest_entry {
        Some(entry) => entry.verify_file(dest.as_std_path()).is_ok(),
        None => true,
    }
}

/// Verify `dest`; on failure delete it (so it can't be treated as a resumable
/// partial later) and return the error.
fn verify_or_discard(manifest_entry: Option<&ManifestEntry>, dest: &Utf8Path) -> Result<()> {
    if let Some(entry) = manifest_entry
        && let Err(e) = entry.verify_file(dest.as_std_path())
    {
        let _ = std::fs::remove_file(dest.as_std_path());
        return Err(Error::Verify {
            filename: dest.file_name().unwrap_or("?").to_owned(),
            reason: e.to_string(),
        });
    }
    Ok(())
}

/// Stream a response body into `file` to completion, then flush.
async fn stream_to_file(
    url: &str,
    response: reqwest::Response,
    file: &mut tokio::fs::File,
    dest: &Utf8Path,
) -> Result<()> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Network {
            url: url.to_owned(),
            source: e,
        })?;
        file.write_all(&chunk).await.map_err(|e| Error::Io {
            path: dest.to_path_buf().into_std_path_buf(),
            source: e,
        })?;
    }
    file.flush().await.map_err(|e| Error::Io {
        path: dest.to_path_buf().into_std_path_buf(),
        source: e,
    })?;
    Ok(())
}

/// Expose a distfile found in a read-only distdir under the writable `dest`
/// (in DISTDIR), so the build's unpack/eapply — which only consult DISTDIR —
/// can open it. Best-effort, mirroring portage: prefer a symlink to the RO
/// copy, fall back to a hard link, then a copy; replaces any stale entry.
fn link_into_distdir(src: &Utf8Path, dest: &Utf8Path) {
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent.as_std_path());
    }
    let _ = std::fs::remove_file(dest.as_std_path());
    if std::os::unix::fs::symlink(src.as_std_path(), dest.as_std_path()).is_ok() {
        return;
    }
    if std::fs::hard_link(src.as_std_path(), dest.as_std_path()).is_ok() {
        return;
    }
    let _ = std::fs::copy(src.as_std_path(), dest.as_std_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_only_strict_size_partials() {
        // A genuine under-size partial is resumable.
        assert!(is_resumable(Some(1000), 400));
        // Nothing on disk yet → download fresh.
        assert!(!is_resumable(Some(1000), 0));
        // Complete-but-wrong (corrupt full file) → refetch fresh, don't append.
        assert!(!is_resumable(Some(1000), 1000));
        // Over-size garbage → refetch fresh.
        assert!(!is_resumable(Some(1000), 1500));
        // Unknown manifest size → never blind-resume.
        assert!(!is_resumable(None, 400));
        // The psmisc case: a 139 KB body vs a 432 KB target is *size*-plausible, so
        // a resume is attempted — but the caller always falls back to a fresh
        // download (and discards) when that resume fails to verify.
        assert!(is_resumable(Some(432208), 139065));
    }

    #[test]
    fn link_into_distdir_symlinks_ro_copy_and_replaces_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let base = Utf8Path::from_path(tmp.path()).unwrap();
        let ro = base.join("ro");
        let dist = base.join("dist");
        std::fs::create_dir_all(ro.as_std_path()).unwrap();
        std::fs::create_dir_all(dist.as_std_path()).unwrap();

        let src = ro.join("bash53-001");
        std::fs::write(src.as_std_path(), b"PATCH").unwrap();
        let dest = dist.join("bash53-001");

        // Fresh DISTDIR: a symlink to the RO copy is created and readable.
        link_into_distdir(&src, &dest);
        let meta = std::fs::symlink_metadata(dest.as_std_path()).unwrap();
        assert!(meta.file_type().is_symlink(), "should be a symlink");
        assert_eq!(std::fs::read(dest.as_std_path()).unwrap(), b"PATCH");

        // A stale DISTDIR entry is replaced (not left pointing elsewhere).
        std::fs::remove_file(dest.as_std_path()).unwrap();
        std::fs::write(dest.as_std_path(), b"STALE").unwrap();
        link_into_distdir(&src, &dest);
        assert_eq!(std::fs::read(dest.as_std_path()).unwrap(), b"PATCH");
    }
}
