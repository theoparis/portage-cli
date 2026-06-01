use blake2::{Blake2b512, Digest as _};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{Manifest, ManifestEntry};
use sha2::Sha512;
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
#[derive(Debug, Clone, Default)]
pub struct FetchConfig {
    /// Primary fetch strategy.  Defaults to `Builtin`.
    pub strategy: FetchStrategy,
    /// Fallback command template used when the primary strategy fails.
    ///
    /// Typically set from `FETCHCOMMAND` in `make.conf`.
    /// When `None`, failures are reported directly.
    pub fallback_command: Option<String>,
    /// Resume command template (`RESUMECOMMAND`).
    ///
    /// When `None`, partial files are re-downloaded from scratch.
    pub resume_command: Option<String>,
}

impl FetchConfig {
    /// Build from `make.conf`-style environment/config values.
    pub fn from_make_conf(
        fetch_command: Option<String>,
        resume_command: Option<String>,
    ) -> Self {
        // If FETCHCOMMAND is set, use it as primary; reqwest becomes the
        // fallback for clarity.  If unset, reqwest is always primary.
        match fetch_command {
            Some(cmd) => Self {
                strategy: FetchStrategy::Command(cmd),
                fallback_command: None,
                resume_command,
            },
            None => Self {
                strategy: FetchStrategy::Builtin,
                fallback_command: None,
                resume_command,
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
}

/// Downloads and verifies distfiles.
pub struct Fetcher {
    client: reqwest::Client,
    distdir: Utf8PathBuf,
    config: FetchConfig,
}

impl Fetcher {
    pub fn new(distdir: Utf8PathBuf, config: FetchConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            distdir,
            config,
        }
    }

    /// Fetch a single distfile, verifying it against `manifest`.
    ///
    /// If the file already exists and passes verification it is not
    /// re-downloaded.  If a partial file is present a resume is attempted.
    pub async fn fetch_distfile(
        &self,
        df: &Distfile,
        manifest: &Manifest,
    ) -> Result<FetchStatus> {
        let dest = self.distdir.join(&df.filename);

        let manifest_entry = manifest_entry_for(manifest, &df.filename);

        // Fast path: already present and valid.
        if dest.exists() {
            if let Some(entry) = manifest_entry {
                if entry.verify_file(dest.as_std_path()).is_ok() {
                    return Ok(FetchStatus::AlreadyPresent);
                }
            } else if dest.is_file() {
                // No manifest entry to verify against — treat as present.
                return Ok(FetchStatus::AlreadyPresent);
            }
        }

        if df.urls.is_empty() {
            return Err(Error::AllFailed { filename: df.filename.clone() });
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
                    // Verify after command download.
                    if let Some(entry) = manifest_entry {
                        entry.verify_file(dest.as_std_path()).map_err(|e| Error::Verify {
                            filename: df.filename.clone(),
                            reason: e.to_string(),
                        })?;
                    }
                    return Ok(FetchStatus::Downloaded);
                }
                last_err = result.err();
            }
        }

        Err(last_err.unwrap_or(Error::AllFailed { filename: df.filename.clone() }))
    }

    /// Fetch all distfiles, returning per-file results.
    pub async fn fetch_all<'a>(
        &self,
        distfiles: &'a [Distfile],
        manifest: &Manifest,
    ) -> Vec<(&'a Distfile, Result<FetchStatus>)> {
        let mut results = Vec::with_capacity(distfiles.len());
        for df in distfiles {
            let r = self.fetch_distfile(df, manifest).await;
            results.push((df, r));
        }
        results
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
            FetchStrategy::Builtin => {
                self.fetch_builtin(url, dest, manifest_entry).await
            }
            FetchStrategy::Command(template) => {
                let r = self.run_command(template, url, dest.file_name().unwrap_or(""), dest).await;
                if r.is_ok() {
                    if let Some(entry) = manifest_entry {
                        entry.verify_file(dest.as_std_path()).map_err(|e| Error::Verify {
                            filename: dest.file_name().unwrap_or("?").to_owned(),
                            reason: e.to_string(),
                        })?;
                    }
                }
                r
            }
        }
    }

    async fn fetch_builtin(
        &self,
        url: &str,
        dest: &Utf8Path,
        manifest_entry: Option<&ManifestEntry>,
    ) -> Result<()> {
        // Check for a partial file to resume.
        let existing_size = if dest.exists() {
            std::fs::metadata(dest.as_std_path())
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        // If there's a resume command and a partial file, try it first.
        if existing_size > 0 {
            if let Some(resume_tmpl) = &self.config.resume_command {
                let r = self.run_command(resume_tmpl, url, dest.file_name().unwrap_or(""), dest).await;
                if r.is_ok() {
                    if let Some(entry) = manifest_entry {
                        if entry.verify_file(dest.as_std_path()).is_ok() {
                            return Ok(());
                        }
                        // Verification failed after resume — fall through to fresh download.
                    } else {
                        return Ok(());
                    }
                }
            }
            // No resume command: use HTTP Range header.
        }

        // Build request, possibly with Range header for resume.
        let mut req = self.client.get(url);
        if existing_size > 0 && self.config.resume_command.is_none() {
            req = req.header("Range", format!("bytes={existing_size}-"));
        }

        let response = req.send().await.map_err(|e| Error::Network {
            url: url.to_owned(),
            source: e,
        })?;

        let status = response.status();
        // 200 OK (fresh) or 206 Partial Content (resume) are both fine.
        if !status.is_success() {
            return Err(Error::Http {
                url: url.to_owned(),
                status: status.as_u16(),
            });
        }

        // Stream body into file, simultaneously feeding SHA512 + BLAKE2B.
        let resuming = status == reqwest::StatusCode::PARTIAL_CONTENT && existing_size > 0;
        let mut file = if resuming {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(dest.as_std_path())
                .await
                .map_err(|e| Error::Io { path: dest.to_path_buf().into_std_path_buf(), source: e })?
        } else {
            tokio::fs::File::create(dest.as_std_path())
                .await
                .map_err(|e| Error::Io { path: dest.to_path_buf().into_std_path_buf(), source: e })?
        };

        let mut sha512 = Sha512::new();
        let mut blake2b = Blake2b512::new();

        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Network { url: url.to_owned(), source: e })?;
            sha512.update(&chunk);
            blake2b.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| Error::Io { path: dest.to_path_buf().into_std_path_buf(), source: e })?;
        }
        file.flush()
            .await
            .map_err(|e| Error::Io { path: dest.to_path_buf().into_std_path_buf(), source: e })?;

        // Verify against manifest if available.
        if let Some(entry) = manifest_entry {
            entry.verify_file(dest.as_std_path()).map_err(|e| Error::Verify {
                filename: dest.file_name().unwrap_or("?").to_owned(),
                reason: e.to_string(),
            })?;
        }

        Ok(())
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
