use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("provider: {0}")]
    Provider(String),
    #[error("capture: {0}")]
    Capture(String),
    #[error("image: {0}")]
    Image(String),
    #[error("unknown session: {0}")]
    UnknownSession(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Machine-readable failure kind for the D-Bus `Failed` signal.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Provider(_) => "provider",
            Error::Capture(_) => "capture",
            Error::Image(_) => "capture",
            _ => "internal",
        }
    }
}
