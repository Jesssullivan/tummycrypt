//! tcfs-storage: OpenDAL storage abstraction + SeaweedFS native API

pub mod health;
pub mod multipart;
pub mod operator;
pub mod seaweedfs;

pub use health::check_health;
pub use operator::{build_operator, StorageConfig};
