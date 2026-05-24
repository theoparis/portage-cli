/// Error type for portage-metadata parsing and operations.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Invalid EAPI value.
    #[error("invalid EAPI: {0}")]
    InvalidEapi(String),

    /// Invalid keyword string.
    #[error("invalid keyword: {0}")]
    InvalidKeyword(String),

    /// Invalid IUSE flag entry.
    #[error("invalid IUSE entry: {0}")]
    InvalidIUse(String),

    /// Invalid phase function name.
    #[error("invalid phase: {0}")]
    InvalidPhase(String),

    /// Invalid SRC_URI expression.
    #[error("invalid SRC_URI: {0}")]
    InvalidSrcUri(String),

    /// Invalid LICENSE expression.
    #[error("invalid LICENSE: {0}")]
    InvalidLicense(String),

    /// Invalid REQUIRED_USE expression.
    #[error("invalid REQUIRED_USE: {0}")]
    InvalidRequiredUse(String),

    /// Invalid RESTRICT or PROPERTIES expression.
    #[error("invalid RESTRICT/PROPERTIES: {0}")]
    InvalidRestrict(String),

    /// Error parsing a metadata cache entry.
    #[error("invalid cache entry: {0}")]
    InvalidCacheEntry(String),

    /// Missing mandatory field in a cache entry.
    #[error("missing required field: {0}")]
    MissingField(String),

    /// Error from the portage-atom dependency parser.
    #[error("dependency parse error: {0}")]
    DepError(String),

    /// Invalid SLOT value (does not conform to PMS 3.1.3).
    #[error("invalid SLOT: {0}")]
    InvalidSlot(String),
}

/// Result type for portage-metadata operations.
pub type Result<T> = std::result::Result<T, Error>;
