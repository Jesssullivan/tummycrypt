pub mod config;
pub mod error;
pub mod types;

pub use error::{TcfsError, TcfsResult};

/// Generated gRPC types and service traits (from tcfs.proto)
pub mod proto {
    tonic::include_proto!("tcfs");
}
