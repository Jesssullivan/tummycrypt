//! Integration test: git-bundle sync → peer rehydrate preserves history.
//!
//! Verifies the bundle-mode path end-to-end:
//!   1. Create a temp git repo with >=2 commits.
//!   2. Push the tree in bundle mode (sync_git_dirs=true, git_sync_mode=bundle).
//!      The raw `.git/*` internals are NOT walked; instead a single
//!      `.git-tcfs-bundle` object is synced.
//!   3. Pull every synced object into a fresh peer directory.
//!   4. Restore git history from the bundle on the peer.
//!   5. Assert `git log` shows both commits and `git status` is clean.

use opendal::Operator;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn memory_operator() -> Operator {
    Operator::new(opendal::services::Memory::default())
        .expect("memory operator")
        .finish()
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("running git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("running git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn init_repo_with_two_commits(dir: &Path) {
    git(dir, &["init", "--quiet", "-b", "main"]);
    git(dir, &["config", "user.email", "test@tcfs.local"]);
    git(dir, &["config", "user.name", "TCFS Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);

    std::fs::write(dir.join("README.md"), b"first\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", "first commit"]);

    std::fs::write(dir.join("file2.txt"), b"second\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", "second commit"]);
}

#[tokio::test]
async fn git_bundle_mode_preserves_history_on_peer() {
    // Skip gracefully if git is unavailable in the environment.
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git not available; skipping git_bundle_mode_preserves_history_on_peer");
        return;
    }

    let src_tmp = TempDir::new().unwrap();
    let repo = src_tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo_with_two_commits(&repo);

    let op = memory_operator();
    let prefix = "test/git-bundle";

    let mut state = tcfs_sync::state::StateCache::open(&src_tmp.path().join("state.db")).unwrap();

    // Bundle mode: include .git, but capture it as a bundle (not raw walk).
    let config = tcfs_sync::engine::CollectConfig {
        sync_git_dirs: true,
        git_sync_mode: "bundle".into(),
        sync_empty_dirs: false,
        ..Default::default()
    };

    // Sanity: the collector must NOT walk raw `.git/*` internals, but MUST
    // include the synthesized bundle object.
    let collected = tcfs_sync::engine::collect_files(&repo, &config).unwrap();
    let rels: Vec<String> = collected
        .files
        .iter()
        .map(|p| p.strip_prefix(&repo).unwrap().to_string_lossy().to_string())
        .collect();
    assert!(
        rels.iter().any(|r| r == ".git-tcfs-bundle"),
        "bundle object should be in collected set: {rels:?}"
    );
    assert!(
        !rels.iter().any(|r| r.starts_with(".git/")),
        "raw .git internals must not be walked in bundle mode: {rels:?}"
    );

    let (uploaded, _skipped, _bytes) = tcfs_sync::engine::push_tree_with_device(
        &op,
        &repo,
        prefix,
        &mut state,
        None,
        "neo",
        Some(&config),
        None,
    )
    .await
    .expect("push tree in bundle mode");
    assert!(
        uploaded >= 3,
        "expected README, file2, bundle: got {uploaded}"
    );

    // ── Peer rehydrate ───────────────────────────────────────────────────
    let peer_tmp = TempDir::new().unwrap();
    let peer_repo = peer_tmp.path().join("repo");
    std::fs::create_dir_all(&peer_repo).unwrap();

    let mut restore_state =
        tcfs_sync::state::StateCache::open(&peer_tmp.path().join("restore-state.db")).unwrap();

    // Pull every synced working-tree path (incl. the bundle) into the peer.
    for rel in &rels {
        let manifest = tcfs_sync::engine::resolve_manifest_path(&op, rel, prefix, Some(&repo))
            .await
            .unwrap_or_else(|e| panic!("resolve manifest for {rel}: {e}"));
        let dst = peer_repo.join(rel);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        tcfs_sync::engine::download_file_with_device(
            &op,
            &manifest,
            &dst,
            prefix,
            None,
            "honey",
            Some(&mut restore_state),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("download {rel}: {e}"));
    }

    // Before restore the peer has working-tree files but no `.git` history.
    assert!(peer_repo.join("README.md").exists());
    assert!(peer_repo.join("file2.txt").exists());
    assert!(peer_repo.join(".git-tcfs-bundle").exists());
    assert!(!peer_repo.join(".git").exists());

    // Restore git history from the bundle.
    let restored = tcfs_sync::engine::restore_git_bundles_under(peer_tmp.path());
    assert_eq!(restored, 1, "exactly one repo should be restored");

    // ── Assertions: history + clean status on the peer ─────────────────────
    let log = git_stdout(&peer_repo, &["log", "--oneline"]);
    assert!(
        log.contains("first commit"),
        "peer git log should include first commit: {log}"
    );
    assert!(
        log.contains("second commit"),
        "peer git log should include second commit: {log}"
    );
    let count = log.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(count, 2, "peer should have exactly 2 commits: {log}");

    // `git status` should be clean. The TCFS bundle artifact is the only
    // untracked file; ignore it so the working tree is otherwise pristine.
    let status = git_stdout(
        &peer_repo,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    let dirty: Vec<&str> = status
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !l.ends_with(".git-tcfs-bundle"))
        .collect();
    assert!(
        dirty.is_empty(),
        "peer working tree should be clean (excluding bundle artifact): {dirty:?}"
    );

    // HEAD should resolve to the same branch the source was on.
    let branch = git_stdout(&peer_repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch.trim(), "main", "peer HEAD should be on main");
}
