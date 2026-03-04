//! tcfs-storage: OpenDAL storage abstraction + SeaweedFS native API

pub mod health;
pub mod multipart;
pub mod operator;
pub mod seaweedfs;

pub use health::check_health;
pub use operator::{build_operator, StorageConfig};

/// Parse a remote spec like `seaweedfs://host:port/bucket[/prefix]`
///
/// Returns `(endpoint, bucket, prefix)` where:
/// - endpoint: `http://host:port`
/// - bucket: first path component
/// - prefix: remaining path (may be empty)
pub fn parse_remote_spec(spec: &str) -> anyhow::Result<(String, String, String)> {
    let rest = spec.strip_prefix("seaweedfs://").ok_or_else(|| {
        anyhow::anyhow!("remote spec must start with seaweedfs:// — got: {}", spec)
    })?;

    // Split host:port from /bucket[/prefix]
    let slash = rest
        .find('/')
        .ok_or_else(|| anyhow::anyhow!("remote spec must include /bucket — got: {}", spec))?;

    let host = &rest[..slash]; // e.g. "dees-appu-bearts:8333"
    let path = &rest[slash + 1..]; // e.g. "tcfs-test" or "tcfs-test/subdir"

    // First path component = bucket, remainder = prefix
    let (bucket, prefix) = path.split_once('/').unwrap_or((path, ""));

    Ok((
        format!("http://{}", host),
        bucket.to_string(),
        prefix.trim_end_matches('/').to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_remote_spec_basic() {
        let (ep, bucket, prefix) = parse_remote_spec("seaweedfs://host:8333/mybucket").unwrap();
        assert_eq!(ep, "http://host:8333");
        assert_eq!(bucket, "mybucket");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_parse_remote_spec_with_prefix() {
        let (ep, bucket, prefix) =
            parse_remote_spec("seaweedfs://host:8333/mybucket/data").unwrap();
        assert_eq!(ep, "http://host:8333");
        assert_eq!(bucket, "mybucket");
        assert_eq!(prefix, "data");
    }

    #[test]
    fn test_parse_remote_spec_nested_prefix() {
        let (_, _, prefix) = parse_remote_spec("seaweedfs://host:8333/bucket/a/b/c").unwrap();
        assert_eq!(prefix, "a/b/c");
    }

    #[test]
    fn test_parse_remote_spec_trailing_slash() {
        let (_, _, prefix) = parse_remote_spec("seaweedfs://host:8333/bucket/data/").unwrap();
        assert_eq!(prefix, "data");
    }

    #[test]
    fn test_parse_remote_spec_bad_scheme() {
        assert!(parse_remote_spec("s3://host/bucket").is_err());
    }

    #[test]
    fn test_parse_remote_spec_no_bucket() {
        assert!(parse_remote_spec("seaweedfs://host").is_err());
    }
}
