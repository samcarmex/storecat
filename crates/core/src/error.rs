use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("index open error: {0}")]
    TantivyOpen(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("index read error: {0}")]
    TantivyRead(#[from] tantivy::directory::error::OpenReadError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("volume {0} not found")]
    VolumeNotFound(i64),
    #[error("volume is not connected")]
    VolumeOffline,
    #[error("a scan is already running for this volume")]
    ScanInProgress,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}
