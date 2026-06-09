//! E2E test: `.git`-aware fast-forward conflict resolution (R3).
//!
//! Reproduces the bidirectional `.git` roam fast-forward case:
//!   1. Two devices share a raw-synced git repo at commit C0.
//!   2. Device B commits C1 (C0 is an ancestor of C1 — a strict fast-forward),
//!      advancing `.git/refs/heads/main` + `.git/index` + `.git/logs`.
//!   3. Without the reclassifier those `.git/*` paths each compare as a vclock
//!      Conflict (both devices ticked their own clock). The FF reclassifier must
//!      instead reclassify them: B's reconcile PUSHES (LocalNewer), and a peer
//!      that is behind PULLS (RemoteNewer).
//!   4. A DIVERGENT case (B@C1, A@C2, neither an ancestor of the other) must
//!      STILL produce a Conflict (the reclassifier is fail-closed).
//!
//! These tests drive the real `reconcile()` pipeline in raw git-sync mode
//! against a memory operator, mirroring the patterns in
//! `e2e_two_device_sync.rs` / `git_bundle_roundtrip.rs`.

use std::path::Path;
use std::process::Command;

use opendal::Operator;
use tempfile::TempDir;

use tcfs_sync::blacklist::Blacklist;
use tcfs_sync::engine::{push_tree_with_device, CollectConfig};
use tcfs_sync::reconcile::{execute_plan, reconcile, ReconcileAction, ReconcileConfig};
use tcfs_sync::state::StateCache;

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

fn init_repo_c0(dir: &Path) {
    git(dir, &["init", "--quiet", "-b", "main"]);
    git(dir, &["config", "user.email", "test@tcfs.local"]);
    git(dir, &["config", "user.name", "TCFS Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), b"c0\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", "C0"]);
}

fn commit(dir: &Path, file: &str, content: &[u8], msg: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", msg]);
    git_stdout(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn head_sha(dir: &Path) -> String {
    git_stdout(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn raw_blacklist() -> Blacklist {
    // sync_git_dirs = true, git_sync_mode = "raw"
    Blacklist::new(&[], false, true, "raw")
}

fn raw_collect_config() -> CollectConfig {
    CollectConfig {
        sync_git_dirs: true,
        git_sync_mode: "raw".into(),
        sync_empty_dirs: false,
        preserve_symlinks: true,
        ..Default::default()
    }
}

fn ff_config() -> ReconcileConfig {
    ReconcileConfig {
        git_sync_mode: "raw".into(),
        git_ff_resolution: true,
        ..Default::default()
    }
}

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn fsck_clean(dir: &Path) {
    let out = Command::new("git")
        .args(["fsck", "--full"])
        .current_dir(dir)
        .output()
        .expect("git fsck");
    assert!(
        out.status.success(),
        "git fsck --full not clean in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Count Conflict actions whose path is inside a `.git` dir.
fn git_conflicts(plan: &tcfs_sync::reconcile::ReconcilePlan) -> Vec<String> {
    plan.actions
        .iter()
        .filter_map(|a| match a {
            ReconcileAction::Conflict { rel_path, .. }
                if rel_path.contains(".git/") || rel_path == ".git" =>
            {
                Some(rel_path.clone())
            }
            _ => None,
        })
        .collect()
}

fn git_ref_push(plan: &tcfs_sync::reconcile::ReconcilePlan) -> bool {
    plan.actions.iter().any(|a| {
        matches!(
            a,
            ReconcileAction::Push { rel_path, .. } if rel_path.ends_with(".git/refs/heads/main")
        )
    })
}

fn git_ref_pull(plan: &tcfs_sync::reconcile::ReconcilePlan) -> bool {
    plan.actions.iter().any(|a| {
        matches!(
            a,
            ReconcileAction::Pull { rel_path, .. } if rel_path.ends_with(".git/refs/heads/main")
        )
    })
}

/// Fast-forward: B advances to C1, A is still at C0. B must PUSH (LocalNewer),
/// A must PULL (RemoteNewer); both converge on C1 and fsck clean.
#[tokio::test]
async fn git_ff_converges_push_then_pull() {
    if !git_available() {
        eprintln!("git not available; skipping git_ff_converges_push_then_pull");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff";

    // ── Device A: repo at C0, raw-sync the whole tree (incl. .git/*). ─────────
    let a_tmp = TempDir::new().unwrap();
    let a_repo = a_tmp.path().join("repo");
    std::fs::create_dir_all(&a_repo).unwrap();
    init_repo_c0(&a_repo);
    let c0 = head_sha(&a_repo);

    let mut a_state = StateCache::open(&a_tmp.path().join("a.db")).unwrap();
    let collect = raw_collect_config();
    push_tree_with_device(
        &op,
        &a_repo,
        prefix,
        &mut a_state,
        None,
        "device-a",
        Some(&collect),
        None,
    )
    .await
    .expect("device-a initial raw push");
    a_state.flush().unwrap();

    // ── Device B: pull the repo (NewRemote) into an empty dir. ────────────────
    let b_tmp = TempDir::new().unwrap();
    let b_repo = b_tmp.path().join("repo");
    std::fs::create_dir_all(&b_repo).unwrap();
    let mut b_state = StateCache::open(&b_tmp.path().join("b.db")).unwrap();
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    let b_pull_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("device-b pull plan");
    execute_plan(
        &b_pull_plan,
        &op,
        &b_repo,
        prefix,
        &mut b_state,
        "device-b",
        None,
        None,
    )
    .await
    .expect("device-b pull execute");
    b_state.flush().unwrap();

    // B now has the repo at C0; its .git is raw-restored. Objects + refs roamed,
    // so HEAD resolves to C0.
    assert_eq!(head_sha(&b_repo), c0, "device-b should be at C0 after pull");
    fsck_clean(&b_repo);

    // ── Device B commits C1 (a strict fast-forward over C0). ──────────────────
    let c1 = commit(&b_repo, "feature.txt", b"c1\n", "C1");
    assert_ne!(c1, c0);

    // B reconciles: the .git/* paths each "conflict" on raw vclock, but the FF
    // reclassifier must turn them into PUSH (B is strictly ahead).
    let b_push_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("device-b FF push plan");

    assert!(
        git_conflicts(&b_push_plan).is_empty(),
        "FF: no .git conflicts expected on B, got {:?}",
        git_conflicts(&b_push_plan)
    );
    assert!(
        git_ref_push(&b_push_plan),
        "FF: B must push .git/refs/heads/main (LocalNewer)"
    );

    execute_plan(
        &b_push_plan,
        &op,
        &b_repo,
        prefix,
        &mut b_state,
        "device-b",
        None,
        None,
    )
    .await
    .expect("device-b FF push execute");
    b_state.flush().unwrap();

    // ── Device A (still at C0) reconciles: must PULL to C1 (RemoteNewer). ──────
    let a_pull_plan = reconcile(
        &op, &a_repo, prefix, &a_state, "device-a", &blacklist, &cfg, None,
    )
    .await
    .expect("device-a FF pull plan");

    assert!(
        git_conflicts(&a_pull_plan).is_empty(),
        "FF: no .git conflicts expected on A, got {:?}",
        git_conflicts(&a_pull_plan)
    );
    assert!(
        git_ref_pull(&a_pull_plan),
        "FF: A must pull .git/refs/heads/main (RemoteNewer)"
    );

    execute_plan(
        &a_pull_plan,
        &op,
        &a_repo,
        prefix,
        &mut a_state,
        "device-a",
        None,
        None,
    )
    .await
    .expect("device-a FF pull execute");
    a_state.flush().unwrap();

    // ── Both sides converge on C1, fsck clean. ────────────────────────────────
    assert_eq!(head_sha(&b_repo), c1, "B should be at C1");
    assert_eq!(
        head_sha(&a_repo),
        c1,
        "A should have fast-forwarded to C1 after pull"
    );
    fsck_clean(&a_repo);
    fsck_clean(&b_repo);
}

/// Pull the whole tree from `op`/`prefix` into `repo` under `device`, returning
/// the populated state cache. Used to seed two peers from a common C0 so each
/// has its own tracked vclock (and therefore independently ticks the ref clock
/// when it later commits — the precondition for a genuine concurrent conflict).
async fn pull_into(
    op: &Operator,
    repo: &Path,
    prefix: &str,
    device: &str,
    db: &Path,
    blacklist: &Blacklist,
    cfg: &ReconcileConfig,
) -> StateCache {
    let mut state = StateCache::open(db).unwrap();
    let plan = reconcile(op, repo, prefix, &state, device, blacklist, cfg, None)
        .await
        .expect("pull plan");
    execute_plan(&plan, op, repo, prefix, &mut state, device, None, None)
        .await
        .expect("pull execute");
    state.flush().unwrap();
    state
}

/// Commit locally, then reconcile + execute to push the advance. Returns the
/// resulting plan so callers can assert on its actions.
#[allow(clippy::too_many_arguments)]
async fn commit_and_sync(
    op: &Operator,
    repo: &Path,
    prefix: &str,
    device: &str,
    state: &mut StateCache,
    blacklist: &Blacklist,
    cfg: &ReconcileConfig,
    file: &str,
    content: &[u8],
    msg: &str,
) -> tcfs_sync::reconcile::ReconcilePlan {
    commit(repo, file, content, msg);
    let plan = reconcile(op, repo, prefix, state, device, blacklist, cfg, None)
        .await
        .expect("post-commit plan");
    execute_plan(&plan, op, repo, prefix, state, device, None, None)
        .await
        .expect("post-commit execute");
    state.flush().unwrap();
    plan
}

/// Force the local tracked vclock for `rel_path` under `repo` to carry an extra
/// independent tick from `device`. This models the case where the device has
/// locally advanced the ref (its own write) without yet observing the remote's
/// concurrent advance — the precondition for a genuine *concurrent* vclock
/// conflict (as opposed to the strictly-dominated remote-newer case that the
/// single-key vclock layer otherwise collapses to).
fn tick_tracked_vclock(state: &mut StateCache, rel_path: &str, device: &str) {
    let (key, existing) = state
        .get_by_rel_path(rel_path)
        .map(|(k, s)| (k.to_string(), s.clone()))
        .unwrap_or_else(|| panic!("no tracked state for {rel_path}"));
    let mut updated = existing;
    updated.vclock.tick(device);
    state.set(Path::new(&key), updated);
    state.flush().unwrap();
}

/// Divergent: B advances to C1 and A advances to C2 off the same C0 base —
/// siblings, neither an ancestor of the other. We make A's tracked ref clock
/// concurrent with the remote (B's) clock so the live reconcile produces a
/// genuine `.git/refs/heads/main` Conflict, and fetch B's objects into A so the
/// ancestry probe is fully computable. The FF reclassifier must FAIL CLOSED:
/// ancestry is `NotFastForward`, so the head-ref Conflict is left in place
/// (never reclassified to push/pull).
#[tokio::test]
async fn git_divergent_stays_conflict() {
    if !git_available() {
        eprintln!("git not available; skipping git_divergent_stays_conflict");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-divergent";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // ── Seed device S: repo at C0, raw-synced to the remote. ──────────────────
    let s_tmp = TempDir::new().unwrap();
    let s_repo = s_tmp.path().join("repo");
    std::fs::create_dir_all(&s_repo).unwrap();
    init_repo_c0(&s_repo);
    let mut s_state = StateCache::open(&s_tmp.path().join("s.db")).unwrap();
    let collect = raw_collect_config();
    push_tree_with_device(
        &op,
        &s_repo,
        prefix,
        &mut s_state,
        None,
        "device-s",
        Some(&collect),
        None,
    )
    .await
    .expect("seed raw push");
    s_state.flush().unwrap();

    // ── A and B both pull C0. ─────────────────────────────────────────────────
    let a_tmp = TempDir::new().unwrap();
    let a_repo = a_tmp.path().join("repo");
    std::fs::create_dir_all(&a_repo).unwrap();
    let mut a_state = pull_into(
        &op,
        &a_repo,
        prefix,
        "device-a",
        &a_tmp.path().join("a.db"),
        &blacklist,
        &cfg,
    )
    .await;

    let b_tmp = TempDir::new().unwrap();
    let b_repo = b_tmp.path().join("repo");
    std::fs::create_dir_all(&b_repo).unwrap();
    let mut b_state = pull_into(
        &op,
        &b_repo,
        prefix,
        "device-b",
        &b_tmp.path().join("b.db"),
        &blacklist,
        &cfg,
    )
    .await;

    // ── B commits C1 and pushes it (remote head advances to C1). ──────────────
    commit_and_sync(
        &op,
        &b_repo,
        prefix,
        "device-b",
        &mut b_state,
        &blacklist,
        &cfg,
        "from_b.txt",
        b"b-branch\n",
        "C1 on B",
    )
    .await;
    let c1 = head_sha(&b_repo);

    // ── A commits its OWN sibling C2. ─────────────────────────────────────────
    commit(&a_repo, "from_a.txt", b"a-branch\n", "C2 on A");
    let c2 = head_sha(&a_repo);
    assert_ne!(c1, c2);

    // Bring B's C1 objects into A so the ancestry probe can run with BOTH commits
    // present locally (otherwise a missing object would itself force a defer —
    // still fail-closed, but we want to prove the *neither-ancestor* branch).
    git(
        &a_repo,
        &[
            "fetch",
            "--quiet",
            &b_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    // Neither tip is an ancestor of the other — a true divergence.
    let anc_1 = Command::new("git")
        .args([
            "-C",
            &a_repo.to_string_lossy(),
            "merge-base",
            "--is-ancestor",
            &c1,
            &c2,
        ])
        .output()
        .unwrap();
    let anc_2 = Command::new("git")
        .args([
            "-C",
            &a_repo.to_string_lossy(),
            "merge-base",
            "--is-ancestor",
            &c2,
            &c1,
        ])
        .output()
        .unwrap();
    assert!(
        anc_1.status.code() != Some(0) && anc_2.status.code() != Some(0),
        "test setup: C1 and C2 must be divergent siblings"
    );

    // Make A's tracked ref clock concurrent with the remote (B's) clock: A has
    // an independent tick B never saw, and B's pushed clock has a tick A never
    // saw → `compare_clocks` yields Conflict (not RemoteNewer).
    tick_tracked_vclock(&mut a_state, ".git/refs/heads/main", "device-a");

    let a_plan = reconcile(
        &op, &a_repo, prefix, &a_state, "device-a", &blacklist, &cfg, None,
    )
    .await
    .expect("A divergent plan");

    let conflicts = git_conflicts(&a_plan);
    assert!(
        conflicts
            .iter()
            .any(|c| c.ends_with(".git/refs/heads/main")),
        "divergent: .git/refs/heads/main must stay a Conflict, got conflicts {:?}",
        conflicts
    );
    // It must NOT have been reclassified into a head-ref push or pull.
    assert!(
        !git_ref_push(&a_plan) && !git_ref_pull(&a_plan),
        "divergent: the head ref must not be reclassified to push/pull"
    );

    drop(a_state);
}
