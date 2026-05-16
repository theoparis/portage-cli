use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

pub type Result<T> = std::result::Result<T, Error>;
