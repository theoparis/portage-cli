//! Parallel ebuild sourcing.
//!
//! - [`source_parallel`] — stream all ebuilds through a worker pool, callback per result
//! - [`source_single`] — source one ebuild (emerge cache-miss path)
//!
//! Both share a [`SourceContext`] that holds the eclass AST cache. Pass the
//! same instance across calls within a single run to maximise cache hits.
//! No disk I/O is performed here; writing cache files is `crate::regen`.

use std::sync::Arc;

use camino::Utf8PathBuf;
use portage_metadata::EbuildMetadata;

use crate::{Ebuild, Repository, Result};

/// Result of sourcing an ebuild.
///
/// Bundles the extracted metadata with the resolved file paths of every
/// eclass that was sourced (in inheritance order). The paths come from the
/// `inherit` builtin's own resolution against the live eclass search dirs,
/// so they correctly reflect eclasses pulled from master repositories rather
/// than the local repo — which a name-based lookup at write time cannot.
#[derive(Debug, Clone)]
pub struct SourcedEbuild {
    /// Parsed metadata variables (DEPEND, IUSE, EAPI, …).
    pub metadata: EbuildMetadata,
    /// Eclasses sourced for this ebuild, paired `(name, file path)`.
    pub eclasses: Vec<(String, Utf8PathBuf)>,
}

type AstCache = Arc<papaya::HashMap<String, brush_parser::ast::Program>>;

/// Shared eclass AST cache.
///
/// Create once per run and pass the same instance to every sourcing call.
/// The internal brush AST types are not part of the public API.
#[derive(Clone, Default)]
pub struct SourceContext(pub(crate) AstCache);

impl SourceContext {
    /// Create a fresh (empty) sourcing context.
    pub fn new() -> Self {
        Self(Arc::new(papaya::HashMap::new()))
    }
}

/// Options for sourcing operations.
#[derive(Debug, Clone, Default)]
pub struct SourceOpts {
    /// Number of parallel workers. `None` uses [`std::thread::available_parallelism`].
    pub jobs: Option<usize>,
    /// Deduplicate top-level dep tokens before returning metadata.
    pub dedup: bool,
}

/// Source `ebuilds` in parallel, calling `on_result` for each outcome.
///
/// Results arrive in completion order (not submission order). The run
/// continues on per-ebuild errors; failures are delivered through `Err`.
pub async fn source_parallel<F>(
    repo: &Repository,
    masters: &[Repository],
    ebuilds: Vec<Ebuild>,
    opts: &SourceOpts,
    ctx: &SourceContext,
    on_result: F,
) -> Result<()>
where
    F: Fn(Ebuild, Result<SourcedEbuild>) + Send + Sync + 'static,
{
    let jobs = opts.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let dedup = opts.dedup;
    let repo = Arc::new(repo.clone());
    let masters: Arc<Vec<Repository>> = Arc::new(masters.to_vec());
    let ast_cache = ctx.0.clone();
    let on_result = Arc::new(on_result);

    let (tx, rx) = flume::bounded::<Ebuild>(jobs * 2);

    let mut handles = Vec::with_capacity(jobs);
    for _ in 0..jobs {
        let rx = rx.clone();
        let repo = Arc::clone(&repo);
        let masters = Arc::clone(&masters);
        let ast_cache = Arc::clone(&ast_cache);
        let on_result = Arc::clone(&on_result);

        handles.push(tokio::spawn(async move {
            let master_refs: Vec<&Repository> = masters.iter().collect();
            while let Ok(ebuild) = rx.recv_async().await {
                let result = source_one(&repo, &master_refs, &ebuild, &ast_cache, dedup).await;
                on_result(ebuild, result);
            }
        }));
    }
    drop(rx);

    for ebuild in ebuilds {
        let _ = tx.send_async(ebuild).await;
    }
    drop(tx);

    for h in handles {
        let _ = h.await;
    }

    Ok(())
}

/// Source a single ebuild and return its metadata.
///
/// Use this for the emerge cache-miss path where spawning a full worker pool
/// would be wasteful. Reuse the same `ctx` across multiple calls in one run.
pub async fn source_single(
    repo: &Repository,
    masters: &[Repository],
    ebuild: &Ebuild,
    ctx: &SourceContext,
) -> Result<SourcedEbuild> {
    let master_refs: Vec<&Repository> = masters.iter().collect();
    source_one(repo, &master_refs, ebuild, &ctx.0, false).await
}

pub(crate) async fn source_one(
    repo: &Repository,
    masters: &[&Repository],
    ebuild: &Ebuild,
    ast_cache: &AstCache,
    dedup: bool,
) -> Result<SourcedEbuild> {
    let mut shell = repo
        .shell_with_masters_and_cache(masters, Arc::clone(ast_cache))
        .await?;
    let mut sourced = shell.source_ebuild(ebuild).await?;
    if dedup {
        sourced.metadata = sourced.metadata.dedup();
    }
    Ok(sourced)
}
