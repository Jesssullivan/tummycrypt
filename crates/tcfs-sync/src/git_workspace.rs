//! Strict adapter for Bulkload's GitWorkspaceV2 capture contract.
//!
//! Bulkload owns capture and Git reconstruction. TCFS consumes the reviewed
//! logical topology and working-file identities only; device-local Git admin
//! paths are deliberately private and never returned as transport inputs.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub const AGENT_CAPTURE_SCHEMA: &str = "dev.tinyland.bulkload.agent-capture.v4";
pub const GIT_WORKSPACE_SCHEMA: &str = "dev.tinyland.bulkload.git-workspace.v2";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitWorkspaceV2 {
    schema: String,
    pub workspace_id: String,
    pub logical_path: String,
    #[serde(rename = "path")]
    _source_path: String,
    pub destination_path: PathBuf,
    #[serde(rename = "common_git_dir")]
    _common_git_dir: String,
    pub object_format: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub remotes: Vec<GitRemote>,
    pub refs: Vec<GitRef>,
    pub recovery_anchors: Vec<RecoveryAnchor>,
    object_files: Vec<FileRecord>,
    pub worktrees: Vec<GitWorktree>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitRef {
    pub name: String,
    pub oid: String,
    pub symbolic_target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAnchor {
    pub oid: String,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    classification: String,
    relative_path: String,
    kind: String,
    mode: String,
    size: u64,
    sha256: Option<String>,
    #[serde(default)]
    status: Option<GitStatus>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitStatus {
    pub conflicted: bool,
    pub missing: bool,
    pub modified: bool,
    pub path: String,
    pub staged: bool,
    pub untracked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitWorktree {
    #[serde(rename = "path")]
    _source_path: String,
    pub destination_path: PathBuf,
    #[serde(rename = "git_dir")]
    _git_dir: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
    index: GitIndex,
    files: Vec<FileRecord>,
    pub dirt: Vec<GitStatus>,
    pub operation_state: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitIndex {
    exists: bool,
    mode: Option<String>,
    #[serde(rename = "path")]
    _path: String,
    sha256: Option<String>,
    size: u64,
    entries: Vec<GitIndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitIndexEntry {
    assume_unchanged: bool,
    intent_to_add: bool,
    mode: String,
    oid: String,
    path: String,
    skip_worktree: bool,
    stage: u8,
}

/// One reviewed working-tree byte selected for TCFS transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorkspaceFile {
    pub logical_path: String,
    pub local_path: PathBuf,
    pub kind: String,
    pub mode: String,
    pub size: u64,
    pub sha256: Option<String>,
}

impl GitWorkspaceV2 {
    /// Decode one workspace embedded in a complete Bulkload AgentCaptureV4.
    pub fn from_agent_capture(bytes: &[u8], workspace_id: &str) -> Result<Self> {
        let capture: Value = serde_json::from_slice(bytes).context("parsing Bulkload capture")?;
        anyhow::ensure!(
            capture.get("schema").and_then(Value::as_str) == Some(AGENT_CAPTURE_SCHEMA),
            "input is not Bulkload AgentCaptureV4"
        );
        anyhow::ensure!(
            matches!(
                capture.get("role").and_then(Value::as_str),
                Some("source" | "destination")
            ),
            "Bulkload capture role is invalid"
        );
        anyhow::ensure!(
            capture.get("writers_quiesced").and_then(Value::as_bool) == Some(true),
            "Bulkload capture lacks writer quiescence"
        );
        require_object_digest(&capture, "capture_sha256")?;
        anyhow::ensure!(
            capture.get("complete").and_then(Value::as_bool) == Some(true),
            "Bulkload capture is incomplete"
        );
        let catalog = capture
            .get("catalog")
            .and_then(Value::as_object)
            .context("Bulkload capture catalog is missing")?;
        let catalog_value = Value::Object(catalog.clone());
        let expected_catalog = sha256_json(&catalog_value)?;
        anyhow::ensure!(
            capture.get("catalog_sha256").and_then(Value::as_str)
                == Some(expected_catalog.as_str()),
            "Bulkload capture catalog digest mismatch"
        );
        let blockers = catalog
            .get("blockers")
            .and_then(Value::as_array)
            .context("Bulkload capture blockers are missing")?;
        anyhow::ensure!(blockers.is_empty(), "Bulkload capture contains blockers");
        anyhow::ensure!(
            capture.get("complete").and_then(Value::as_bool) == Some(blockers.is_empty()),
            "Bulkload capture completeness disagrees with blockers"
        );
        let candidates = catalog
            .get("git_workspaces")
            .and_then(Value::as_array)
            .context("Bulkload capture git_workspaces are missing")?;
        let mut matches = candidates.iter().filter(|candidate| {
            candidate.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
        });
        let selected = matches
            .next()
            .with_context(|| format!("GitWorkspaceV2 '{workspace_id}' is absent"))?;
        anyhow::ensure!(
            matches.next().is_none(),
            "GitWorkspaceV2 '{workspace_id}' is duplicated"
        );
        let workspace: Self = serde_json::from_value(selected.clone())
            .context("decoding strict GitWorkspaceV2 record")?;
        workspace.validate()?;
        Ok(workspace)
    }

    /// Return only logical working bytes. Raw Git admin/object/index paths are
    /// intentionally not exposed by this adapter.
    pub fn transport_files(&self) -> Vec<GitWorkspaceFile> {
        let mut result = Vec::new();
        for (worktree_index, _) in self.worktrees.iter().enumerate() {
            result.extend(
                self.transport_files_for_worktree(worktree_index)
                    .expect("enumerated worktree index is valid"),
            );
        }
        result.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
        result
    }

    /// Select only one device-local worktree from a possibly multi-worktree
    /// workspace. Other linked worktree destinations are topology evidence,
    /// not children of this registered root.
    pub fn transport_files_for_worktree(
        &self,
        worktree_index: usize,
    ) -> Result<Vec<GitWorkspaceFile>> {
        let worktree = self
            .worktrees
            .get(worktree_index)
            .context("GitWorkspaceV2 worktree index is out of range")?;
        let mut result = worktree
            .files
            .iter()
            .map(|file| GitWorkspaceFile {
                logical_path: format!(
                    "{}/worktrees/{worktree_index}/{}",
                    self.logical_path.trim_end_matches('/'),
                    file.relative_path
                ),
                local_path: worktree.destination_path.join(&file.relative_path),
                kind: file.kind.clone(),
                mode: file.mode.clone(),
                size: file.size,
                sha256: file.sha256.clone(),
            })
            .collect::<Vec<_>>();
        result.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
        Ok(result)
    }

    /// Exact relative namespace authorized for one worktree. Tracked missing
    /// paths come from the typed index; present files/directories come from the
    /// capture. The generated bundle is the sole TCFS-owned addition.
    pub fn transport_paths_for_worktree(&self, worktree_index: usize) -> Result<BTreeSet<String>> {
        let worktree = self
            .worktrees
            .get(worktree_index)
            .context("GitWorkspaceV2 worktree index is out of range")?;
        let mut paths = worktree
            .files
            .iter()
            .map(|file| file.relative_path.clone())
            .chain(
                worktree
                    .index
                    .entries
                    .iter()
                    .map(|entry| entry.path.clone()),
            )
            .collect::<BTreeSet<_>>();
        paths.insert(".git-tcfs-bundle".to_string());
        Ok(paths)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema == GIT_WORKSPACE_SCHEMA,
            "unknown Git workspace schema"
        );
        validate_hex(&self.workspace_id, 64, "workspace_id")?;
        validate_relative(&self.logical_path, true)?;
        validate_absolute(&self.destination_path, "destination_path")?;
        let oid_len = match self.object_format.as_str() {
            "sha1" => 40,
            "sha256" => 64,
            other => bail!("unsupported Git object format {other:?}"),
        };
        if let Some(head) = &self.head {
            validate_hex(head, oid_len, "head")?;
        }
        if let Some(branch) = &self.branch {
            validate_ref_name(branch)?;
        }

        let mut names = BTreeSet::new();
        for remote in &self.remotes {
            anyhow::ensure!(
                names.insert(remote.name.as_str()),
                "duplicate Git remote name"
            );
            validate_remote(remote)?;
        }
        let mut refs = BTreeSet::new();
        for git_ref in &self.refs {
            validate_ref_name(&git_ref.name)?;
            validate_hex(&git_ref.oid, oid_len, "ref oid")?;
            if let Some(target) = &git_ref.symbolic_target {
                validate_ref_name(target)?;
            }
            anyhow::ensure!(refs.insert(git_ref.name.as_str()), "duplicate Git ref");
        }
        for anchor in &self.recovery_anchors {
            validate_hex(&anchor.oid, oid_len, "recovery anchor oid")?;
            for source in &anchor.sources {
                validate_relative(source, false)?;
            }
        }
        for file in &self.object_files {
            validate_file(file, "git-object")?;
            anyhow::ensure!(
                file.status.is_none(),
                "Git object record carries worktree status"
            );
        }
        anyhow::ensure!(!self.worktrees.is_empty(), "Git workspace has no worktrees");
        for worktree in &self.worktrees {
            worktree.validate(oid_len)?;
        }
        Ok(())
    }
}

fn sha256_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value).context("canonicalizing Bulkload JSON")?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn require_object_digest(value: &Value, digest_field: &str) -> Result<()> {
    let object = value
        .as_object()
        .context("Bulkload capture must be a JSON object")?;
    let actual = object
        .get(digest_field)
        .and_then(Value::as_str)
        .with_context(|| format!("Bulkload capture lacks {digest_field}"))?;
    validate_hex(actual, 64, digest_field)?;
    let mut unsealed = object.clone();
    unsealed.remove(digest_field);
    anyhow::ensure!(
        actual == sha256_json(&Value::Object(unsealed))?,
        "Bulkload capture {digest_field} mismatch"
    );
    Ok(())
}

impl GitWorktree {
    fn validate(&self, oid_len: usize) -> Result<()> {
        validate_absolute(&self.destination_path, "worktree destination_path")?;
        anyhow::ensure!(
            self.operation_state.is_empty(),
            "Git worktree has an active operation"
        );
        if let Some(head) = &self.head {
            validate_hex(head, oid_len, "worktree head")?;
        }
        if let Some(branch) = &self.branch {
            validate_ref_name(branch)?;
        }
        if self.index.exists {
            validate_mode(
                self.index
                    .mode
                    .as_deref()
                    .context("Git index mode is missing")?,
            )?;
            validate_hex(
                self.index
                    .sha256
                    .as_deref()
                    .context("Git index digest is missing")?,
                64,
                "Git index sha256",
            )?;
            anyhow::ensure!(self.index.size > 0, "existing Git index is empty");
        } else {
            anyhow::ensure!(
                self.index.mode.is_none() && self.index.sha256.is_none() && self.index.size == 0,
                "missing Git index carries content metadata"
            );
        }
        for entry in &self.index.entries {
            validate_relative(&entry.path, false)?;
            anyhow::ensure!(entry.stage <= 3, "Git index stage is invalid");
            anyhow::ensure!(
                matches!(
                    entry.mode.as_str(),
                    "100644" | "100755" | "120000" | "160000"
                ),
                "Git index mode is invalid"
            );
            if entry.intent_to_add {
                anyhow::ensure!(
                    entry.oid.bytes().all(|byte| byte == b'0'),
                    "intent-to-add oid is not zero"
                );
            } else {
                validate_hex(&entry.oid, oid_len, "Git index oid")?;
            }
            let _flags = (entry.assume_unchanged, entry.skip_worktree);
        }
        for file in &self.files {
            validate_file(file, "git-worktree")?;
            if file.kind == "directory" {
                anyhow::ensure!(
                    file.status.is_none(),
                    "worktree directory carries file status"
                );
            } else {
                let status = file
                    .status
                    .as_ref()
                    .context("worktree file status is missing")?;
                anyhow::ensure!(
                    status.path == file.relative_path,
                    "worktree status path mismatch"
                );
            }
        }
        for status in &self.dirt {
            validate_relative(&status.path, false)?;
            anyhow::ensure!(
                status.conflicted
                    || status.missing
                    || status.modified
                    || status.staged
                    || status.untracked,
                "clean path appears in Git dirt inventory"
            );
        }
        Ok(())
    }
}

fn validate_file(file: &FileRecord, classification: &str) -> Result<()> {
    anyhow::ensure!(
        file.classification == classification,
        "unexpected file classification"
    );
    validate_relative(&file.relative_path, false)?;
    anyhow::ensure!(
        !file
            .relative_path
            .split('/')
            .any(|segment| segment.eq_ignore_ascii_case(".git")),
        "raw Git administrative paths are never TCFS transport inputs"
    );
    validate_mode(&file.mode)?;
    match file.kind.as_str() {
        "regular" | "symlink" => {
            validate_hex(
                file.sha256.as_deref().context("file digest is missing")?,
                64,
                "file sha256",
            )?;
        }
        "directory" => {
            anyhow::ensure!(
                file.sha256.is_none() && file.size == 0,
                "directory carries content"
            );
        }
        other => bail!("unsupported Git workspace file kind {other:?}"),
    }
    Ok(())
}

fn validate_remote(remote: &GitRemote) -> Result<()> {
    anyhow::ensure!(
        !remote.name.is_empty() && !remote.name.chars().any(char::is_whitespace),
        "invalid Git remote name"
    );
    let value = remote.url.as_str();
    anyhow::ensure!(
        !value.is_empty() && !value.contains(['\r', '\n', '\0', '?', '#']),
        "Git remote is not sanitized"
    );
    if let Some(digest) = value.strip_prefix("local-path:sha256:") {
        return validate_hex(digest, 64, "local remote digest");
    }
    anyhow::ensure!(!value.contains('@'), "Git remote contains userinfo");
    let allowed_url = ["git://", "http://", "https://", "ssh://"]
        .iter()
        .any(|prefix| value.starts_with(prefix));
    let allowed_scp = !value.contains("://") && value.split_once(':').is_some();
    anyhow::ensure!(
        allowed_url || allowed_scp,
        "unsupported sanitized Git remote"
    );
    Ok(())
}

fn validate_ref_name(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.starts_with("refs/")
            && !value.contains("..")
            && !value.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
            && !value.ends_with('/')
            && !value.ends_with(".lock"),
        "invalid Git ref name {value:?}"
    );
    Ok(())
}

fn validate_mode(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() == 4 && value.bytes().all(|byte| matches!(byte, b'0'..=b'7')),
        "invalid portable file mode"
    );
    Ok(())
}

fn validate_hex(value: &str, length: usize, label: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "invalid {label}"
    );
    Ok(())
}

fn validate_relative(value: &str, allow_dot: bool) -> Result<()> {
    if allow_dot && value == "." {
        return Ok(());
    }
    anyhow::ensure!(
        !value.is_empty()
            && !value.starts_with('/')
            && !value.contains('\\')
            && value
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != ".."),
        "invalid portable relative path {value:?}"
    );
    Ok(())
}

fn validate_absolute(path: &Path, label: &str) -> Result<()> {
    anyhow::ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|component| matches!(component, Component::ParentDir)),
        "{label} must be absolute without '..': {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(complete: bool, operation: &str, unknown_field: bool) -> Vec<u8> {
        let one = "1".repeat(40);
        let digest = "a".repeat(64);
        let operation_state = if operation.is_empty() {
            Vec::<String>::new()
        } else {
            vec![operation.to_string()]
        };
        let mut workspace = serde_json::json!({
            "schema": GIT_WORKSPACE_SCHEMA,
            "workspace_id": digest,
            "logical_path": "repo",
            "path": "/source/repo",
            "destination_path": "/dest/repo",
            "common_git_dir": "/source/repo/.git",
            "object_format": "sha1",
            "head": one,
            "branch": "refs/heads/main",
            "remotes": [{"name": "origin", "url": "https://example.invalid/repo.git"}],
            "refs": [{"name": "refs/heads/main", "oid": one, "symbolic_target": null}],
            "recovery_anchors": [{"oid": one, "sources": ["logs/HEAD"]}],
            "object_files": [{"classification": "git-object", "relative_path": "pack/a.pack", "kind": "regular", "mode": "0444", "size": 1, "sha256": digest}],
            "worktrees": [{
                "path": "/source/repo",
                "destination_path": "/dest/repo",
                "git_dir": "/source/repo/.git",
                "head": one,
                "branch": "refs/heads/main",
                "detached": false,
                "locked": false,
                "prunable": false,
                "index": {"exists": true, "mode": "0644", "path": "/source/repo/.git/index", "sha256": digest, "size": 1, "entries": [{"assume_unchanged": false, "intent_to_add": false, "mode": "100644", "oid": one, "path": "README.md", "skip_worktree": false, "stage": 0}]},
                "files": [
                    {"classification": "git-worktree", "relative_path": "docs", "kind": "directory", "mode": "0755", "size": 0, "sha256": null},
                    {"classification": "git-worktree", "relative_path": "README.md", "kind": "regular", "mode": "0644", "size": 1, "sha256": digest, "status": {"conflicted": false, "missing": false, "modified": false, "path": "README.md", "staged": false, "untracked": false}}
                ],
                "dirt": [],
                "operation_state": operation_state
            }]
        });
        if unknown_field {
            workspace["parallel_transport"] = serde_json::json!(true);
        }
        let catalog = serde_json::json!({"blockers": [], "git_workspaces": [workspace]});
        let catalog_sha256 = sha256_json(&catalog).unwrap();
        let mut capture = serde_json::json!({
            "schema": AGENT_CAPTURE_SCHEMA,
            "complete": complete,
            "role": "source",
            "writers_quiesced": true,
            "catalog": catalog,
            "catalog_sha256": catalog_sha256
        });
        let capture_sha256 = sha256_json(&capture).unwrap();
        capture["capture_sha256"] = serde_json::json!(capture_sha256);
        serde_json::to_vec(&capture).unwrap()
    }

    #[test]
    fn consumes_only_complete_known_git_workspace_v2() {
        let digest = "a".repeat(64);
        let workspace = GitWorkspaceV2::from_agent_capture(&capture(true, "", false), &digest)
            .expect("valid workspace");
        let files = workspace.transport_files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].local_path, Path::new("/dest/repo/README.md"));
        assert_eq!(files[1].local_path, Path::new("/dest/repo/docs"));
        assert!(!files[0].local_path.to_string_lossy().contains("/.git/"));
        let paths = workspace.transport_paths_for_worktree(0).unwrap();
        assert!(paths.contains("README.md"));
        assert!(paths.contains(".git-tcfs-bundle"));
        assert!(!paths.contains(".git"));
    }

    #[test]
    fn rejects_incomplete_unknown_or_operating_capture() {
        let digest = "a".repeat(64);
        assert!(GitWorkspaceV2::from_agent_capture(&capture(false, "", false), &digest).is_err());
        assert!(GitWorkspaceV2::from_agent_capture(&capture(true, "", true), &digest).is_err());
        assert!(
            GitWorkspaceV2::from_agent_capture(&capture(true, "rebase", false), &digest).is_err()
        );

        let mut tampered: Value = serde_json::from_slice(&capture(true, "", false)).unwrap();
        tampered["catalog"]["git_workspaces"][0]["head"] = serde_json::json!("2".repeat(40));
        assert!(GitWorkspaceV2::from_agent_capture(
            &serde_json::to_vec(&tampered).unwrap(),
            &digest
        )
        .is_err());
    }
}
