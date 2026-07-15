//! Distfile fetching and verification for Gentoo Portage.
//!
//! Resolves `SRC_URI` entries to mirror URLs, downloads them honoring
//! `DISTDIR`/`PORTAGE_RO_DISTDIRS`, and verifies them against Manifest
//! checksums.

pub mod binhost;
pub mod binhost_cache;
pub mod error;
pub mod fetch;
pub mod mirrors;
pub mod resolver;

pub use binhost::{IndexFetch, fetch_binpkg, fetch_index};
pub use binhost_cache::fetch_index_cached;
pub use error::{Error, Result};
pub use fetch::{FetchConfig, FetchStatus, FetchStrategy, Fetcher};
pub use mirrors::{Endpoint, Mirror, MirrorList, default_mirror_list};
pub use resolver::{Distfile, DistfileResolver, collect_filenames};
