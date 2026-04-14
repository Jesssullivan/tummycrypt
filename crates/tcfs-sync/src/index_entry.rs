use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// A parsed remote index entry that points to a committed manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIndexEntry {
    pub manifest_hash: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub chunks: usize,
}

/// State for a versioned index entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexEntryState {
    Committed,
    Preparing,
}

/// Pending manifest metadata recorded while a path publish is in-flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingIndexEntry {
    pub manifest_hash: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub chunks: usize,
    pub staged_manifest_key: String,
}

/// Fully parsed index entry, supporting both the legacy text format and the
/// planned versioned JSON format for durability work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedIndexEntry {
    Legacy(RemoteIndexEntry),
    V2(VersionedIndexEntry),
}

/// Versioned JSON index entry used by the #224 durability design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedIndexEntry {
    pub state: IndexEntryState,
    pub current: Option<RemoteIndexEntry>,
    pub pending: Option<PendingIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VersionedIndexEntryWire {
    version: u8,
    state: IndexEntryState,
    #[serde(default)]
    current: Option<RemoteIndexEntry>,
    #[serde(default)]
    pending: Option<PendingIndexEntry>,
}

impl ParsedIndexEntry {
    pub fn state(&self) -> IndexEntryState {
        match self {
            ParsedIndexEntry::Legacy(_) => IndexEntryState::Committed,
            ParsedIndexEntry::V2(entry) => entry.state,
        }
    }

    /// Return the currently visible manifest pointer for path-based reads.
    ///
    /// For legacy entries this is the only entry. For versioned entries this is
    /// the committed/current pointer, which may be absent for a brand-new path
    /// that is still in a `preparing` state.
    pub fn visible_entry(&self) -> Option<&RemoteIndexEntry> {
        match self {
            ParsedIndexEntry::Legacy(entry) => Some(entry),
            ParsedIndexEntry::V2(entry) => entry.current.as_ref(),
        }
    }

    pub fn pending_entry(&self) -> Option<&PendingIndexEntry> {
        match self {
            ParsedIndexEntry::Legacy(_) => None,
            ParsedIndexEntry::V2(entry) => entry.pending.as_ref(),
        }
    }
}

/// Parse the current visible remote index entry for callers that only support
/// committed path pointers today.
pub fn parse_index_entry(data: &[u8]) -> Result<RemoteIndexEntry> {
    parse_index_entry_record(data)?
        .visible_entry()
        .cloned()
        .context("index entry has no visible current manifest")
}

/// Parse a remote index entry from either the legacy text format or the
/// versioned JSON format planned for crash-safe publish.
pub fn parse_index_entry_record(data: &[u8]) -> Result<ParsedIndexEntry> {
    let text = std::str::from_utf8(data).context("index entry is not valid UTF-8")?;
    let trimmed = text.trim_start();

    if trimmed.starts_with('{') {
        return parse_versioned_index_entry(trimmed);
    }

    parse_legacy_index_entry(trimmed).map(ParsedIndexEntry::Legacy)
}

fn parse_versioned_index_entry(text: &str) -> Result<ParsedIndexEntry> {
    let wire: VersionedIndexEntryWire =
        serde_json::from_str(text).context("parsing versioned index entry JSON")?;

    if wire.version != 2 {
        bail!("unsupported index entry version: {}", wire.version);
    }

    match wire.state {
        IndexEntryState::Committed => {
            if wire.current.is_none() {
                bail!("committed index entry missing current");
            }
        }
        IndexEntryState::Preparing => {
            if wire.pending.is_none() {
                bail!("preparing index entry missing pending");
            }
        }
    }

    Ok(ParsedIndexEntry::V2(VersionedIndexEntry {
        state: wire.state,
        current: wire.current,
        pending: wire.pending,
    }))
}

fn parse_legacy_index_entry(text: &str) -> Result<RemoteIndexEntry> {
    let mut manifest_hash = None;
    let mut size = 0u64;
    let mut chunks = 0usize;

    for line in text.lines() {
        if let Some(v) = line.strip_prefix("manifest_hash=") {
            manifest_hash = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("size=") {
            size = v.parse().context("invalid size in index entry")?;
        } else if let Some(v) = line.strip_prefix("chunks=") {
            chunks = v.parse().context("invalid chunk count in index entry")?;
        }
    }

    Ok(RemoteIndexEntry {
        manifest_hash: manifest_hash.context("index entry missing manifest_hash")?,
        size,
        chunks,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_index_entry, parse_index_entry_record, IndexEntryState, ParsedIndexEntry};

    #[test]
    fn parse_legacy_index_entry() {
        let data = b"manifest_hash=abc123\nsize=1024\nchunks=2\n";
        let entry = parse_index_entry(data).unwrap();
        assert_eq!(entry.manifest_hash, "abc123");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.chunks, 2);
    }

    #[test]
    fn parse_committed_json_index_entry() {
        let data = br#"{
            "version": 2,
            "state": "committed",
            "current": {
                "manifest_hash": "abc123",
                "size": 1024,
                "chunks": 2
            }
        }"#;

        let parsed = parse_index_entry_record(data).unwrap();
        assert_eq!(parsed.state(), IndexEntryState::Committed);
        let visible = parsed.visible_entry().unwrap();
        assert_eq!(visible.manifest_hash, "abc123");
        assert_eq!(visible.size, 1024);
        assert_eq!(visible.chunks, 2);
        assert!(parsed.pending_entry().is_none());
    }

    #[test]
    fn parse_preparing_json_index_entry() {
        let data = br#"{
            "version": 2,
            "state": "preparing",
            "current": {
                "manifest_hash": "old123",
                "size": 10,
                "chunks": 1
            },
            "pending": {
                "manifest_hash": "new456",
                "size": 11,
                "chunks": 1,
                "staged_manifest_key": "data/staging/manifests/txn-1.json"
            }
        }"#;

        let parsed = parse_index_entry_record(data).unwrap();
        assert_eq!(parsed.state(), IndexEntryState::Preparing);
        let visible = parsed.visible_entry().unwrap();
        assert_eq!(visible.manifest_hash, "old123");

        let pending = parsed.pending_entry().unwrap();
        assert_eq!(pending.manifest_hash, "new456");
        assert_eq!(
            pending.staged_manifest_key,
            "data/staging/manifests/txn-1.json"
        );
    }

    #[test]
    fn preparing_entry_without_current_is_not_visible() {
        let data = br#"{
            "version": 2,
            "state": "preparing",
            "pending": {
                "manifest_hash": "new456",
                "size": 11,
                "chunks": 1,
                "staged_manifest_key": "data/staging/manifests/txn-1.json"
            }
        }"#;

        let parsed = parse_index_entry_record(data).unwrap();
        assert!(parsed.visible_entry().is_none());
        assert!(parse_index_entry(data).is_err());
    }

    #[test]
    fn committed_entry_missing_current_errors() {
        let data = br#"{
            "version": 2,
            "state": "committed"
        }"#;

        assert!(parse_index_entry_record(data).is_err());
    }

    #[test]
    fn preparing_entry_missing_pending_errors() {
        let data = br#"{
            "version": 2,
            "state": "preparing",
            "current": {
                "manifest_hash": "old123",
                "size": 10,
                "chunks": 1
            }
        }"#;

        assert!(parse_index_entry_record(data).is_err());
    }

    #[test]
    fn unsupported_version_errors() {
        let data = br#"{
            "version": 3,
            "state": "committed",
            "current": {
                "manifest_hash": "abc123",
                "size": 1,
                "chunks": 1
            }
        }"#;

        assert!(parse_index_entry_record(data).is_err());
    }

    #[test]
    fn malformed_legacy_size_errors() {
        let data = b"manifest_hash=abc123\nsize=notanumber\nchunks=5\n";
        assert!(parse_index_entry(data).is_err());
    }

    #[test]
    fn malformed_legacy_chunks_errors() {
        let data = b"manifest_hash=abc123\nsize=1024\nchunks=xyz\n";
        assert!(parse_index_entry(data).is_err());
    }

    #[test]
    fn parsed_entry_keeps_v2_shape() {
        let data = br#"{
            "version": 2,
            "state": "committed",
            "current": {
                "manifest_hash": "abc123",
                "size": 1,
                "chunks": 1
            }
        }"#;

        match parse_index_entry_record(data).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected v2 entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert!(entry.current.is_some());
            }
        }
    }
}
