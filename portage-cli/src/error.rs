use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("not implemented: {0}")]
    NotImplemented(String),
    #[error("VDB error: {0}")]
    Vdb(#[from] portage_vdb::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
