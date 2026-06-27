//! Gentoo binary package (GPKG) reading and writing.
//!
//! Currently provides the [GLEP 78](https://www.gentoo.org/glep/glep-0078.html)
//! GPKG container **writer** ([`write_gpkg`]); the reader, the binhost `Packages`
//! index and signing live here as they land. Used by the
//! [`em`](https://github.com/lu-zero/portage-cli) Portage CLI.
//!
//! The writer shells out to GNU `tar` and `zstd` (so file capabilities, ACLs and
//! device nodes in the image survive natively), matching Portage's own approach.
#![forbid(unsafe_code)]

pub mod error;
pub mod gpkg;

pub use error::{Error, Result};
pub use gpkg::{GpkgInput, write_gpkg};
