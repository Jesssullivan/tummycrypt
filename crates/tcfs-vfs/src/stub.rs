//! `.tc` / `.tcf` stub file format — parse and write
//!
//! Stub format (UTF-8, sorted key-value, Unix newlines, max 2048 bytes):
//! ```text
//! version https://tummycrypt.io/tcfs/v1
//! chunks 23
//! compressed 0
//! fetched 0
//! oid blake3:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e239
//! origin seaweedfs://filer.example.com/bucket/path/to/file
//! size 94371840
//! ```
//!
//! Rules:
//! - `version` must be the first line
//! - Keys are sorted alphabetically (except `version`)
//! - One space between key and value
//! - `fetched 0` = stub (not downloaded), `fetched 1` = hydrated
//! - `.tc` = file stub, `.tcf` = directory listing stub

use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Version string for all stubs
pub const STUB_VERSION: &str = "https://tummycrypt.io/tcfs/v1";

/// File extension for file stubs
pub const TC_EXT: &str = ".tc";

/// File extension for directory stubs
pub const TCF_EXT: &str = ".tcf";

/// Metadata extracted from a `.tc` stub file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubMeta {
    /// Number of FastCDC chunks
    pub chunks: usize,
    /// Whether the content is seekable-zstd compressed
    pub compressed: bool,
    /// Whether the content has been fetched/hydrated
    pub fetched: bool,
    /// Content OID: "blake3:{hex}"
    pub oid: String,
    /// Remote origin URL: "seaweedfs://host/bucket/path"
    pub origin: String,
    /// True file size in bytes (as stored remotely)
    pub size: u64,
}

impl StubMeta {
    /// Parse a stub file from its text content.
    pub fn parse(content: &str) -> Result<Self> {
        let mut chunks = None;
        let mut compressed = None;
        let mut fetched = None;
        let mut oid = None;
        let mut origin = None;
        let mut size = None;
        let mut found_version = false;

        for (lineno, line) in content.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once(' ')
                .with_context(|| format!("line {}: no space separator: {:?}", lineno + 1, line))?;

            match key {
                "version" => {
                    if value != STUB_VERSION {
                        anyhow::bail!("unsupported stub version: {}", value);
                    }
                    found_version = true;
                }
                "chunks" => {
                    chunks = Some(
                        value
                            .parse::<usize>()
                            .with_context(|| format!("invalid chunks: {}", value))?,
                    );
                }
                "compressed" => {
                    compressed = Some(value != "0");
                }
                "fetched" => {
                    fetched = Some(value != "0");
                }
                "oid" => {
                    oid = Some(value.to_string());
                }
                "origin" => {
                    origin = Some(value.to_string());
                }
                "size" => {
                    size = Some(
                        value
                            .parse::<u64>()
                            .with_context(|| format!("invalid size: {}", value))?,
                    );
                }
                _ => {
                    // Unknown keys are silently ignored for forward compatibility
                }
            }
        }

        anyhow::ensure!(found_version, "missing version line");

        Ok(StubMeta {
            chunks: chunks.unwrap_or(0),
            compressed: compressed.unwrap_or(false),
            fetched: fetched.unwrap_or(false),
            oid: oid.context("missing oid field")?,
            origin: origin.context("missing origin field")?,
            size: size.context("missing size field")?,
        })
    }

    /// Serialize to the canonical stub wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_stub_string().into_bytes()
    }

    /// Serialize to the canonical stub text format (key-value, sorted).
    pub fn to_stub_string(&self) -> String {
        // version is always first; remaining keys in alphabetical order
        format!(
            "version {version}\nchunks {chunks}\ncompressed {compressed}\nfetched {fetched}\noid {oid}\norigin {origin}\nsize {size}\n",
            version = STUB_VERSION,
            chunks = self.chunks,
            compressed = if self.compressed { "1" } else { "0" },
            fetched = if self.fetched { "1" } else { "0" },
            oid = self.oid,
            origin = self.origin,
            size = self.size,
        )
    }

    /// Extract the BLAKE3 hex hash from the `oid` field ("blake3:{hex}").
    pub fn blake3_hex(&self) -> Option<&str> {
        self.oid.strip_prefix("blake3:")
    }

    /// Build a stub for a file that has been pushed to remote storage.
    pub fn for_upload(
        manifest_hash: &str,
        size: u64,
        chunks: usize,
        remote_prefix: &str,
        rel_path: &str,
    ) -> Self {
        StubMeta {
            chunks,
            compressed: false,
            fetched: false,
            oid: format!("blake3:{}", manifest_hash),
            origin: format!("seaweedfs://{}/{}", remote_prefix, rel_path),
            size,
        }
    }
}

/// Returns true if the path ends with `.tc` or `.tcf`.
pub fn is_stub_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| ext == "tc" || ext == "tcf")
        .unwrap_or(false)
}

/// Convert a stub filename to the real filename by stripping `.tc`.
///
/// `main.go.tc` → `main.go`
pub fn stub_to_real_name(name: &OsStr) -> Option<PathBuf> {
    let s = name.to_str()?;
    let stripped = s.strip_suffix(".tc").or_else(|| s.strip_suffix(".tcf"))?;
    Some(PathBuf::from(stripped))
}

/// Convert a real filename to its stub name by appending `.tc`.
///
/// `main.go` → `main.go.tc`
pub fn real_to_stub_name(name: &OsStr) -> PathBuf {
    let mut s = name.to_os_string();
    s.push(".tc");
    PathBuf::from(s)
}

// ── Index entry format ────────────────────────────────────────────────────────

/// Metadata stored in an index entry at `{prefix}/index/{rel_path}`.
///
/// This is the lightweight record the FUSE driver uses for `getattr` and
/// `readdir` without fetching the full manifest.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub manifest_hash: String,
    pub size: u64,
    pub chunks: usize,
}

impl IndexEntry {
    /// Parse an index entry from its text content.
    pub fn parse(content: &str) -> Result<Self> {
        let mut manifest_hash = None;
        let mut size = None;
        let mut chunks = None;

        for line in content.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k {
                    "manifest_hash" => manifest_hash = Some(v.to_string()),
                    "size" => size = Some(v.parse::<u64>().context("invalid size")?),
                    "chunks" => chunks = Some(v.parse::<usize>().context("invalid chunks")?),
                    _ => {}
                }
            }
        }

        Ok(IndexEntry {
            manifest_hash: manifest_hash.context("missing manifest_hash")?,
            size: size.context("missing size")?,
            chunks: chunks.unwrap_or(0),
        })
    }

    /// Manifest path under `{prefix}/manifests/`.
    pub fn manifest_path(&self, prefix: &str) -> String {
        format!(
            "{}/manifests/{}",
            prefix.trim_end_matches('/'),
            self.manifest_hash
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_STUB: &str = "\
version https://tummycrypt.io/tcfs/v1
chunks 23
compressed 0
fetched 0
oid blake3:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e239
origin seaweedfs://filer.example.com/bucket/path/to/file
size 94371840
";

    #[test]
    fn parse_and_roundtrip() {
        let meta = StubMeta::parse(SAMPLE_STUB).unwrap();
        assert_eq!(meta.chunks, 23);
        assert!(!meta.compressed);
        assert!(!meta.fetched);
        assert_eq!(
            meta.oid,
            "blake3:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e239"
        );
        assert_eq!(
            meta.origin,
            "seaweedfs://filer.example.com/bucket/path/to/file"
        );
        assert_eq!(meta.size, 94_371_840);

        let reserialized = meta.to_stub_string();
        let reparsed = StubMeta::parse(&reserialized).unwrap();
        assert_eq!(meta, reparsed);
    }

    #[test]
    fn blake3_hex_extraction() {
        let meta = StubMeta::parse(SAMPLE_STUB).unwrap();
        assert_eq!(
            meta.blake3_hex(),
            Some("4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e239")
        );
    }

    #[test]
    fn stub_path_detection() {
        assert!(is_stub_path(Path::new("file.go.tc")));
        assert!(is_stub_path(Path::new("dir.tcf")));
        assert!(!is_stub_path(Path::new("file.go")));
        assert!(!is_stub_path(Path::new("file.txt")));
    }

    #[test]
    fn stub_name_conversion() {
        use std::ffi::OsStr;
        let real = stub_to_real_name(OsStr::new("main.go.tc")).unwrap();
        assert_eq!(real, PathBuf::from("main.go"));

        let stub = real_to_stub_name(OsStr::new("main.go"));
        assert_eq!(stub, PathBuf::from("main.go.tc"));
    }

    #[test]
    fn parse_unknown_version_fails() {
        let bad = "version https://other.io/v99\noid blake3:abc\norigin x\nsize 0\n";
        assert!(StubMeta::parse(bad).is_err());
    }

    #[test]
    fn parse_index_entry() {
        let raw = "manifest_hash=abc123\nsize=4096\nchunks=1\n";
        let entry = IndexEntry::parse(raw).unwrap();
        assert_eq!(entry.manifest_hash, "abc123");
        assert_eq!(entry.size, 4096);
        assert_eq!(entry.chunks, 1);
        assert_eq!(entry.manifest_path("mydata"), "mydata/manifests/abc123");
    }
}
