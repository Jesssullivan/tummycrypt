use thiserror::Error;

pub type TcfsResult<T> = Result<T, TcfsError>;

#[derive(Debug, Error)]
pub enum TcfsError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("secrets error: {0}")]
    Secrets(String),

    #[error("FUSE error: {0}")]
    Fuse(String),

    #[error("sync error: {0}")]
    Sync(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
