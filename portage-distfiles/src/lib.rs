//! Distfile fetching and verification for Gentoo Portage.
//!
//! Resolves `SRC_URI` entries to mirror URLs, downloads them honoring
//! `DISTDIR`/`PORTAGE_RO_DISTDIRS`, and verifies them against Manifest
//! checksums.

pub mod error;
pub mod fetch;
pub mod resolver;

pub use error::{Error, Result};
pub use fetch::{FetchConfig, FetchStatus, FetchStrategy, Fetcher};
pub use resolver::{Distfile, DistfileResolver, collect_filenames};
