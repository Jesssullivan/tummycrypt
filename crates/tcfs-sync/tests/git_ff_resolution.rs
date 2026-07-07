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
use tcfs_sync::engine::{push_tree_with_device, upload_file_with_device, CollectConfig};
use tcfs_sync::reconcile::{
    execute_plan, list_remote_index, reconcile, PullReason, PushReason, ReconcileAction,
    ReconcileConfig, ReconcilePlan, ReconcileSummary,
};
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

/// Deterministically reproduce the same-second stat race that a fast CI runner
/// hits naturally: `git commit` rewrites the (fixed 41-byte) head ref within
/// the same wall-clock second as the sync that recorded its state, so the
/// `(size, mtime-seconds)` pair matches the cache and a stat-based quick check
/// alone cannot see the change. Pin the file's mtime to the cached second so
/// the race is exercised on every run, on every platform — the planned push
/// must still land (execute must trust the plan's content hash, not re-derive
/// staleness from stat).
fn pin_mtime_to_cached_second(state: &StateCache, repo: &Path, rel_path: &str) {
    let (_, cached) = state
        .get_by_rel_path(rel_path)
        .unwrap_or_else(|| panic!("no tracked state for {rel_path}"));
    let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(cached.mtime);
    let file = std::fs::File::options()
        .write(true)
        .open(repo.join(rel_path))
        .expect("open ref file to pin mtime");
    file.set_times(std::fs::FileTimes::new().set_modified(mtime))
        .expect("pin ref mtime to cached second");
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
    // The head ref is 41 bytes at C0 and at C1; force the same-second rewrite
    // (what CI's speed produces naturally) so the push cannot be silently
    // skipped by a stat-granularity quick check.
    pin_mtime_to_cached_second(&b_state, &b_repo, ".git/refs/heads/main");

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
    // Same-second rewrite of the fixed-size head ref (see
    // `pin_mtime_to_cached_second`): the push must land regardless.
    pin_mtime_to_cached_second(state, repo, ".git/refs/heads/main");
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

/// Raw git-sync mode with the FF reclassifier turned OFF — used to prove that
/// a Pull/Push seen under `ff_config()` really came from the reclassifier (the
/// plain vclock path yields Conflict for the same state).
fn raw_no_ff_config() -> ReconcileConfig {
    ReconcileConfig {
        git_sync_mode: "raw".into(),
        git_ff_resolution: false,
        ..Default::default()
    }
}

/// Wrap raw actions in a `ReconcilePlan` for driving `execute_plan` directly.
fn plan_with(actions: Vec<ReconcileAction>) -> ReconcilePlan {
    ReconcilePlan {
        actions,
        summary: ReconcileSummary::default(),
        device_id: "test-device".into(),
        generated_at: 0,
    }
}

/// Recursively collect paths under `root` whose file name contains `needle`.
fn paths_containing(root: &Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(needle))
            {
                hits.push(p.clone());
            }
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    hits
}

/// HIGH-2 (PR #513): genuinely CONCURRENT clocks on the head ref with the
/// local repo FF-ahead. Plan-time reclassification alone is not enough — the
/// upload-time conflict veto used to re-derive Conflict from the concurrent
/// clocks and silently skip (`skipped=true`), replanning the same push every
/// cycle forever. The reclassified FF push must dominate the remote clock
/// (merge + tick, justified by the ancestry proof) and actually UPLOAD, and
/// the peer must converge.
#[tokio::test]
async fn git_ff_concurrent_clocks_push_uploads_and_converges() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-concurrent-push";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0.
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

    // A and B both pull C0.
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

    // A commits C1 and pushes: the remote head-ref clock now carries a
    // device-a tick B has never observed.
    commit_and_sync(
        &op,
        &a_repo,
        prefix,
        "device-a",
        &mut a_state,
        &blacklist,
        &cfg,
        "from_a.txt",
        b"a-c1\n",
        "C1 on A",
    )
    .await;
    let c1 = head_sha(&a_repo);

    // B advances PAST C1 via git (fetch + ff), then commits C2 on top — B's
    // local tip is a strict descendant of the remote tip (C1). B's TRACKED
    // clock for the head ref never saw device-a's tick, and the extra
    // device-b tick models B's own local ref writes — so the plan-time
    // comparison is genuinely CONCURRENT (not Equal).
    git(
        &b_repo,
        &[
            "fetch",
            "--quiet",
            &a_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    git(&b_repo, &["reset", "-q", "--hard", &c1]);
    let c2 = commit(&b_repo, "from_b.txt", b"b-c2\n", "C2 on B");
    tick_tracked_vclock(&mut b_state, ".git/refs/heads/main", "device-b");

    let head_manifest_before = list_remote_index(&op, prefix)
        .await
        .expect("remote index before")
        .get(".git/refs/heads/main")
        .expect("remote head ref entry")
        .manifest_hash
        .clone();

    // Plan: the concurrent head-ref conflict must reclassify to a
    // GitFastForward push (local strictly ahead).
    let b_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B concurrent FF plan");
    assert!(
        git_conflicts(&b_plan).is_empty(),
        "concurrent FF: no .git conflicts expected on B, got {:?}",
        git_conflicts(&b_plan)
    );
    assert!(
        b_plan.actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Push {
                rel_path,
                reason: PushReason::GitFastForward { .. },
                ..
            } if rel_path == ".git/refs/heads/main"
        )),
        "concurrent FF: head ref must be a GitFastForward push"
    );

    // Execute: the push must actually UPLOAD (not veto-skip).
    let b_exec = execute_plan(
        &b_plan,
        &op,
        &b_repo,
        prefix,
        &mut b_state,
        "device-b",
        None,
        None,
    )
    .await
    .expect("B concurrent FF execute");
    b_state.flush().unwrap();
    assert!(
        b_exec.errors.is_empty(),
        "concurrent FF execute errors: {:?}",
        b_exec.errors
    );

    let head_manifest_after = list_remote_index(&op, prefix)
        .await
        .expect("remote index after")
        .get(".git/refs/heads/main")
        .expect("remote head ref entry after")
        .manifest_hash
        .clone();
    assert_ne!(
        head_manifest_before, head_manifest_after,
        "HIGH-2: the reclassified FF push must actually upload the head ref \
         (upload-time veto must not silently skip it)"
    );

    // No livelock: replanning B must not produce the same head-ref push again.
    let b_replan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B replan");
    assert!(
        !git_ref_push(&b_replan) && git_conflicts(&b_replan).is_empty(),
        "HIGH-2 livelock: the head-ref push must not be re-planned after a successful upload"
    );

    // Peer converges: A pulls and lands on C2.
    let a_plan = reconcile(
        &op, &a_repo, prefix, &a_state, "device-a", &blacklist, &cfg, None,
    )
    .await
    .expect("A convergence plan");
    execute_plan(
        &a_plan,
        &op,
        &a_repo,
        prefix,
        &mut a_state,
        "device-a",
        None,
        None,
    )
    .await
    .expect("A convergence execute");
    a_state.flush().unwrap();
    assert_eq!(head_sha(&a_repo), c2, "peer A must converge on C2");
    assert_eq!(head_sha(&b_repo), c2, "B stays at C2");
    fsck_clean(&a_repo);
    fsck_clean(&b_repo);
}

/// The RemoteAhead→Pull reclassifier arm: remote FF-ahead with CONCURRENT
/// clocks. With the reclassifier disabled the head ref is a plain vclock
/// Conflict (proving provenance); enabling it must produce a Pull that
/// converges the local repo onto the remote tip.
#[tokio::test]
async fn git_ff_remote_ahead_concurrent_clocks_reclassifies_to_pull() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-remote-ahead";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0; A and B pull.
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

    // B commits C1 and pushes: remote head is now C1 with a device-b tick.
    commit_and_sync(
        &op,
        &b_repo,
        prefix,
        "device-b",
        &mut b_state,
        &blacklist,
        &cfg,
        "from_b.txt",
        b"b-c1\n",
        "C1 on B",
    )
    .await;
    let c1 = head_sha(&b_repo);

    // A stays at C0 but fetches C1's OBJECTS via git (its main ref does not
    // move), so the plan-time ancestry probe can run with both tips present.
    // A's tracked head-ref clock gets an independent device-a tick, making it
    // genuinely CONCURRENT with the remote clock.
    git(
        &a_repo,
        &[
            "fetch",
            "--quiet",
            &b_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    tick_tracked_vclock(&mut a_state, ".git/refs/heads/main", "device-a");

    // Provenance: with the reclassifier OFF this exact state is a Conflict…
    let no_ff = raw_no_ff_config();
    let a_plain_plan = reconcile(
        &op, &a_repo, prefix, &a_state, "device-a", &blacklist, &no_ff, None,
    )
    .await
    .expect("A plain (no-ff) plan");
    assert!(
        git_conflicts(&a_plain_plan)
            .iter()
            .any(|c| c.ends_with(".git/refs/heads/main")),
        "provenance: without the reclassifier the concurrent head ref must be a Conflict"
    );
    assert!(
        !git_ref_pull(&a_plain_plan),
        "provenance: without the reclassifier there must be no head-ref pull"
    );

    // …so the Pull under ff_config can only come from the RemoteAhead arm.
    let a_ff_plan = reconcile(
        &op, &a_repo, prefix, &a_state, "device-a", &blacklist, &cfg, None,
    )
    .await
    .expect("A FF plan");
    assert!(
        git_ref_pull(&a_ff_plan),
        "RemoteAhead arm: the concurrent head ref must reclassify to Pull"
    );
    assert!(
        git_conflicts(&a_ff_plan).is_empty(),
        "RemoteAhead arm: no .git conflicts expected, got {:?}",
        git_conflicts(&a_ff_plan)
    );

    let a_exec = execute_plan(
        &a_ff_plan,
        &op,
        &a_repo,
        prefix,
        &mut a_state,
        "device-a",
        None,
        None,
    )
    .await
    .expect("A FF pull execute");
    a_state.flush().unwrap();
    assert!(
        a_exec.errors.is_empty(),
        "FF pull execute errors: {:?}",
        a_exec.errors
    );
    assert_eq!(
        head_sha(&a_repo),
        c1,
        "A must fast-forward to C1 via the reclassified pull"
    );
    fsck_clean(&a_repo);
}

/// MEDIUM-1 (PR #513), sequential executor: objects-before-refs must be a real
/// BARRIER, not just an ordering. A failed `.git/objects/**` action bars the
/// repo's ref-class actions for the rest of the run — in BOTH directions.
#[tokio::test]
async fn git_object_failure_bars_ref_actions_sequential() {
    let op = memory_operator();

    // ── Pull direction ────────────────────────────────────────────────────
    let prefix = "test/git-barrier-seq-pull";
    // Seed a real remote manifest for the ref path so an un-barred execution
    // WOULD apply it (making the absence assertion meaningful).
    let seed_tmp = TempDir::new().unwrap();
    let seed_ref = seed_tmp.path().join("main-ref");
    std::fs::write(&seed_ref, format!("{}\n", "a".repeat(40))).unwrap();
    let mut seed_state = StateCache::open(&seed_tmp.path().join("seed.db")).unwrap();
    upload_file_with_device(
        &op,
        &seed_ref,
        prefix,
        &mut seed_state,
        None,
        "device-s",
        Some("r/.git/refs/heads/main"),
        None,
    )
    .await
    .expect("seed ref upload");
    let ref_manifest = list_remote_index(&op, prefix)
        .await
        .expect("seeded index")
        .get("r/.git/refs/heads/main")
        .expect("seeded ref entry")
        .manifest_hash
        .clone();

    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("r/.git/objects")).unwrap();
    let plan = plan_with(vec![
        // Object pull that FAILS (manifest does not exist on the remote).
        ReconcileAction::Pull {
            rel_path: "r/.git/objects/ab/feedface".into(),
            manifest_hash: "0".repeat(64),
            size: 1,
            reason: PullReason::RemoteNewer,
        },
        // Ref pull that WOULD succeed — the barrier must defer it.
        ReconcileAction::Pull {
            rel_path: "r/.git/refs/heads/main".into(),
            manifest_hash: ref_manifest,
            size: 41,
            reason: PullReason::RemoteNewer,
        },
    ]);
    let mut state = StateCache::open(&root.path().join("l.db")).unwrap();
    let res = execute_plan(
        &plan,
        &op,
        root.path(),
        prefix,
        &mut state,
        "device-l",
        None,
        None,
    )
    .await
    .expect("barrier pull execute");
    assert_eq!(
        res.errors.len(),
        1,
        "only the object pull may error: {:?}",
        res.errors
    );
    assert!(res.errors[0].0.contains(".git/objects/"));
    assert_eq!(
        res.deferred_git_refs,
        vec!["r/.git/refs/heads/main".to_string()],
        "the ref pull must be recorded as deferred, not errored"
    );
    assert!(
        !root.path().join("r/.git/refs/heads/main").exists(),
        "BARRIER: the ref must NOT be applied after an object pull failure"
    );

    // ── Push direction ────────────────────────────────────────────────────
    let prefix2 = "test/git-barrier-seq-push";
    let root2 = TempDir::new().unwrap();
    std::fs::create_dir_all(root2.path().join("r/.git/refs/heads")).unwrap();
    std::fs::create_dir_all(root2.path().join("r/.git/objects/ab")).unwrap();
    std::fs::write(
        root2.path().join("r/.git/refs/heads/main"),
        format!("{}\n", "b".repeat(40)),
    )
    .unwrap();
    let plan2 = plan_with(vec![
        // Object push that FAILS (local file does not exist).
        ReconcileAction::Push {
            local_path: root2.path().join("r/.git/objects/ab/missing"),
            rel_path: "r/.git/objects/ab/missing".into(),
            reason: PushReason::NewLocal,
        },
        // Ref push that WOULD succeed — the barrier must defer it.
        ReconcileAction::Push {
            local_path: root2.path().join("r/.git/refs/heads/main"),
            rel_path: "r/.git/refs/heads/main".into(),
            reason: PushReason::NewLocal,
        },
    ]);
    let mut state2 = StateCache::open(&root2.path().join("l2.db")).unwrap();
    let res2 = execute_plan(
        &plan2,
        &op,
        root2.path(),
        prefix2,
        &mut state2,
        "device-l",
        None,
        None,
    )
    .await
    .expect("barrier push execute");
    assert_eq!(
        res2.errors.len(),
        1,
        "only the object push may error: {:?}",
        res2.errors
    );
    assert_eq!(
        res2.deferred_git_refs,
        vec!["r/.git/refs/heads/main".to_string()],
        "the ref push must be recorded as deferred, not errored"
    );
    let idx2 = list_remote_index(&op, prefix2).await.expect("push index");
    assert!(
        !idx2.contains_key("r/.git/refs/heads/main"),
        "BARRIER: the ref must NOT be published after an object push failure"
    );
}

/// MEDIUM-1 (PR #513), concurrent fast path: a wave-0 `.git` pull failure must
/// skip that repo's wave-1 ref-class pulls (deferred, not errored).
#[tokio::test]
async fn git_object_failure_bars_ref_pull_concurrent() {
    let op = memory_operator();
    let prefix = "test/git-barrier-concurrent";

    let seed_tmp = TempDir::new().unwrap();
    let seed_ref = seed_tmp.path().join("main-ref");
    std::fs::write(&seed_ref, format!("{}\n", "c".repeat(40))).unwrap();
    let mut seed_state = StateCache::open(&seed_tmp.path().join("seed.db")).unwrap();
    upload_file_with_device(
        &op,
        &seed_ref,
        prefix,
        &mut seed_state,
        None,
        "device-s",
        Some("r/.git/refs/heads/main"),
        None,
    )
    .await
    .expect("seed ref upload");
    let ref_manifest = list_remote_index(&op, prefix)
        .await
        .expect("seeded index")
        .get("r/.git/refs/heads/main")
        .expect("seeded ref entry")
        .manifest_hash
        .clone();

    let root = TempDir::new().unwrap();
    // All-NewRemote pull plan with no encryption/progress → the concurrent
    // fast path executes it (wave 0 = objects, wave 1 = refs).
    let plan = plan_with(vec![
        ReconcileAction::Pull {
            rel_path: "r/.git/objects/ab/feedface".into(),
            manifest_hash: "0".repeat(64),
            size: 1,
            reason: PullReason::NewRemote,
        },
        ReconcileAction::Pull {
            rel_path: "r/.git/refs/heads/main".into(),
            manifest_hash: ref_manifest,
            size: 41,
            reason: PullReason::NewRemote,
        },
    ]);
    let mut state = StateCache::open(&root.path().join("l.db")).unwrap();
    let res = execute_plan(
        &plan,
        &op,
        root.path(),
        prefix,
        &mut state,
        "device-l",
        None,
        None,
    )
    .await
    .expect("concurrent barrier execute");
    assert_eq!(
        res.errors.len(),
        1,
        "only the wave-0 object pull may error: {:?}",
        res.errors
    );
    assert!(res.errors[0].0.contains(".git/objects/"));
    assert_eq!(
        res.deferred_git_refs,
        vec!["r/.git/refs/heads/main".to_string()],
        "the wave-1 ref pull must be recorded as deferred, not errored"
    );
    assert!(
        !root.path().join("r/.git/refs/heads/main").exists(),
        "BARRIER: wave-1 ref must NOT be applied after a wave-0 object failure"
    );
}

/// HIGH-1 (PR #513): plan computation against a multi-repo-style root (the
/// git repo lives in a subdirectory; the sync root itself has NO `.git`) must
/// not fabricate a `.git` directory at the root and must leave no
/// `tcfs-ff*` temp residue under the root or the repo's live `.git`. The
/// remote ref blob needed by the FF reclassifier is downloaded to an
/// ephemeral temp dir outside the sync root and removed before planning
/// returns.
#[tokio::test]
async fn git_ff_planning_fabricates_nothing_under_root() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-multirepo";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S: sync root containing the repo under `proj/`.
    let s_tmp = TempDir::new().unwrap();
    let s_root = s_tmp.path().join("root");
    let s_proj = s_root.join("proj");
    std::fs::create_dir_all(&s_proj).unwrap();
    init_repo_c0(&s_proj);
    let mut s_state = StateCache::open(&s_tmp.path().join("s.db")).unwrap();
    let collect = raw_collect_config();
    push_tree_with_device(
        &op,
        &s_root,
        prefix,
        &mut s_state,
        None,
        "device-s",
        Some(&collect),
        None,
    )
    .await
    .expect("seed multi-repo raw push");
    s_state.flush().unwrap();

    // B pulls the whole root, then commits C1 inside proj → the next plan has
    // a head-ref conflict that engages the FF reclassifier (which must
    // download the remote ref blob somewhere to resolve the remote tip).
    let b_tmp = TempDir::new().unwrap();
    let b_root = b_tmp.path().join("root");
    std::fs::create_dir_all(&b_root).unwrap();
    let b_state = pull_into(
        &op,
        &b_root,
        prefix,
        "device-b",
        &b_tmp.path().join("b.db"),
        &blacklist,
        &cfg,
    )
    .await;
    commit(&b_root.join("proj"), "feature.txt", b"c1\n", "C1");

    let plan = reconcile(
        &op, &b_root, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("multi-repo FF plan");

    // The reclassifier really engaged (remote ref blob was read successfully
    // through the temp path): the nested head ref is planned as a push.
    assert!(
        plan.actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Push { rel_path, .. } if rel_path == "proj/.git/refs/heads/main"
        )),
        "the nested repo's head ref must reclassify to Push"
    );

    // HIGH-1: no fabricated `.git` at the sync root…
    assert!(
        !b_root.join(".git").exists(),
        "planning must not fabricate {}/.git",
        b_root.display()
    );
    // …and no temp residue anywhere under the root (including proj/.git).
    let residue = paths_containing(&b_root, "tcfs-ff");
    assert!(
        residue.is_empty(),
        "planning left tcfs-ff temp residue under the sync root: {residue:?}"
    );

    drop(b_state);
}

/// BLOCKER-1 (PR #513): a genuinely FF-ahead head ref does NOT license
/// force-syncing a DIVERGENT non-head ref (a tag) that shares the repo's
/// conflict group. The head-only ancestry proof never covers tags/stash/
/// packed-refs/remotes, so if ANY such ref-valued path is in the group the
/// whole `.git` group must stay Conflict (fail-closed, zero writes).
#[tokio::test]
async fn git_ff_divergent_tag_vetoes_whole_group() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-tag-veto";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0 (no tag yet); raw-push everything. A and B pull C0.
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

    // B creates a fresh lightweight tag v1 → C0 and PUSHES it, so B now has
    // tracked sync-state for the tag (clock {device-b}). (update-ref bypasses
    // global git config that may force annotated/signed tags.)
    git(&b_repo, &["update-ref", "refs/tags/v1", "HEAD"]);
    let b_tag_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B tag seed plan");
    execute_plan(
        &b_tag_plan,
        &op,
        &b_repo,
        prefix,
        &mut b_state,
        "device-b",
        None,
        None,
    )
    .await
    .expect("B tag seed execute");
    b_state.flush().unwrap();

    // A commits C1 and pushes (remote head clock now carries a device-a tick;
    // the head conflict FF-reclassifies and dominates).
    commit_and_sync(
        &op,
        &a_repo,
        prefix,
        "device-a",
        &mut a_state,
        &blacklist,
        &cfg,
        "from_a.txt",
        b"a-c1\n",
        "C1 on A",
    )
    .await;
    let c1 = head_sha(&a_repo);

    // B fast-forwards main PAST C1 (a strict descendant, C2) with a concurrent
    // clock, and RE-POINTS its own synced tag v1 → C2 WITHOUT re-syncing. Its
    // tracked tag clock is unchanged, so the new tag content vs the remote's C0
    // is an equal-clock DIVERGENT conflict — a ref-valued non-head member of the
    // repo's group.
    git(
        &b_repo,
        &[
            "fetch",
            "--quiet",
            &a_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    git(&b_repo, &["reset", "-q", "--hard", &c1]);
    let c2 = commit(&b_repo, "from_b.txt", b"b-c2\n", "C2 on B");
    git(&b_repo, &["update-ref", "refs/tags/v1", &c2]);
    tick_tracked_vclock(&mut b_state, ".git/refs/heads/main", "device-b");

    let b_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B plan");

    let conflicts = git_conflicts(&b_plan);
    assert!(
        conflicts.iter().any(|c| c.ends_with(".git/refs/heads/main")),
        "BLOCKER-1: the head ref must STAY Conflict (group vetoed by the divergent tag), got {conflicts:?}"
    );
    assert!(
        conflicts.iter().any(|c| c.ends_with(".git/refs/tags/v1")),
        "BLOCKER-1: the divergent tag must STAY Conflict, got {conflicts:?}"
    );
    // Even though the head alone is FF-ahead, the group must NOT reclassify.
    assert!(
        !git_ref_push(&b_plan) && !git_ref_pull(&b_plan),
        "BLOCKER-1: the whole group is fail-closed — no head-ref push/pull"
    );

    drop(a_state);
}

/// BLOCKER-2 (PR #513), PUSH side: an FF push earns its right to dominate the
/// remote clock from the plan-time LOCAL ref SHA. If the local ref MOVES
/// between plan and execute (a mid-cycle commit on this device), the push must
/// DEFER — it must never publish an unproven state under a dominating clock.
/// The remote head ref stays exactly as planned.
#[tokio::test]
async fn git_ff_push_defers_when_local_ref_moves_after_plan() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-push-defer";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0; A and B pull.
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

    // A commits C1 and pushes (remote head clock now carries a device-a tick).
    commit_and_sync(
        &op,
        &a_repo,
        prefix,
        "device-a",
        &mut a_state,
        &blacklist,
        &cfg,
        "from_a.txt",
        b"a-c1\n",
        "C1 on A",
    )
    .await;
    let c1 = head_sha(&a_repo);

    // B advances PAST C1 to C2 (strict descendant) with a concurrent clock.
    git(
        &b_repo,
        &[
            "fetch",
            "--quiet",
            &a_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    git(&b_repo, &["reset", "-q", "--hard", &c1]);
    let c2 = commit(&b_repo, "from_b.txt", b"b-c2\n", "C2 on B");
    tick_tracked_vclock(&mut b_state, ".git/refs/heads/main", "device-b");

    let head_manifest_before = list_remote_index(&op, prefix)
        .await
        .expect("remote index before")
        .get(".git/refs/heads/main")
        .expect("remote head ref entry")
        .manifest_hash
        .clone();

    // Plan: the concurrent head-ref conflict reclassifies to a GitFastForward
    // push (local strictly ahead, pinned at C2).
    let b_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B FF push plan");
    assert!(
        b_plan.actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Push {
                rel_path,
                reason: PushReason::GitFastForward { .. },
                ..
            } if rel_path == ".git/refs/heads/main"
        )),
        "setup: head ref must plan as a GitFastForward push"
    );

    // MUTATE between plan and execute: a mid-cycle commit moves the local head
    // ref OFF the pinned C2. The proof is now stale.
    let c3 = commit(&b_repo, "from_b2.txt", b"b-c3\n", "C3 on B (mid-cycle)");
    assert_ne!(c3, c2);

    // Execute the STALE plan: the FF push must DEFER, not dominate.
    let b_exec = execute_plan(
        &b_plan,
        &op,
        &b_repo,
        prefix,
        &mut b_state,
        "device-b",
        None,
        None,
    )
    .await
    .expect("B stale FF push execute");
    assert!(
        b_exec
            .deferred_git_refs
            .iter()
            .any(|p| p.ends_with(".git/refs/heads/main")),
        "BLOCKER-2 push: the head-ref push must DEFER when the local ref moved, got deferred={:?}",
        b_exec.deferred_git_refs
    );

    // Remote head ref UNCHANGED — no dominating publish of the stale state.
    let head_manifest_after = list_remote_index(&op, prefix)
        .await
        .expect("remote index after")
        .get(".git/refs/heads/main")
        .expect("remote head ref entry after")
        .manifest_hash
        .clone();
    assert_eq!(
        head_manifest_before, head_manifest_after,
        "BLOCKER-2 push: the remote head ref must be untouched when the FF push defers"
    );
}

/// BLOCKER-2 (PR #513), PULL side: a reclassified FF pull overwrites the local
/// ref only while it still matches the plan-time SHA. If a mid-cycle local
/// commit moves the ref between plan and execute, the pull must SKIP — the
/// fresh local commit (and its pointer) must not be silently clobbered.
#[tokio::test]
async fn git_ff_pull_skips_when_local_ref_moves_after_plan() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-pull-skip";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0; A and B pull.
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

    // B commits C1 and pushes: remote head is now C1 with a device-b tick.
    commit_and_sync(
        &op,
        &b_repo,
        prefix,
        "device-b",
        &mut b_state,
        &blacklist,
        &cfg,
        "from_b.txt",
        b"b-c1\n",
        "C1 on B",
    )
    .await;
    let c1 = head_sha(&b_repo);

    // A stays at C0 but fetches C1's objects so the ancestry probe is
    // computable; A's tracked head-ref clock gets an independent device-a tick
    // → genuinely CONCURRENT with the remote clock.
    git(
        &a_repo,
        &[
            "fetch",
            "--quiet",
            &b_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    tick_tracked_vclock(&mut a_state, ".git/refs/heads/main", "device-a");

    // Plan: the concurrent head-ref conflict reclassifies to a Pull (remote
    // strictly ahead), pinned at A's plan-time C0 local SHA.
    let a_plan = reconcile(
        &op, &a_repo, prefix, &a_state, "device-a", &blacklist, &cfg, None,
    )
    .await
    .expect("A FF pull plan");
    assert!(
        git_ref_pull(&a_plan),
        "setup: the concurrent head ref must reclassify to Pull"
    );

    // MUTATE between plan and execute: a mid-cycle LOCAL commit on A moves the
    // head ref OFF the pinned C0 — the fresh work that must NOT be clobbered.
    let c_local = commit(
        &a_repo,
        "a_local.txt",
        b"a-local\n",
        "local mid-cycle commit on A",
    );
    assert_ne!(c_local, c1);

    // Execute the STALE pull plan: the FF pull must SKIP/defer, not overwrite.
    let a_exec = execute_plan(
        &a_plan,
        &op,
        &a_repo,
        prefix,
        &mut a_state,
        "device-a",
        None,
        None,
    )
    .await
    .expect("A stale FF pull execute");
    assert!(
        a_exec
            .deferred_git_refs
            .iter()
            .any(|p| p.ends_with(".git/refs/heads/main")),
        "BLOCKER-2 pull: the head-ref pull must DEFER when the local ref moved, got deferred={:?}",
        a_exec.deferred_git_refs
    );

    // Local ref preserved at the fresh commit — NOT clobbered to the remote tip.
    assert_eq!(
        head_sha(&a_repo),
        c_local,
        "BLOCKER-2 pull: the fresh local commit must survive"
    );
    assert_ne!(
        head_sha(&a_repo),
        c1,
        "BLOCKER-2 pull: the local ref must NOT be overwritten to the remote tip"
    );

    drop(b_state);
    drop(s_state);
}

/// BLOCKER-3 (PR #513): a genuinely FF-ahead SUPERPROJECT head ref does NOT
/// license force-syncing a DIVERGENT submodule branch ref that shares the outer
/// repo's conflict group. `repo_root_for_git_path` splits on the FIRST `.git`,
/// so every `.git/modules/<name>/**` conflict groups under the OUTER repo and
/// would ride its FF dominance — but the head-only ancestry proof never covers
/// submodule refs. Any ref-class path with no provable top-level head must veto
/// the whole `.git` group (fail-closed, zero writes) so a divergent submodule
/// pointer (branch/tag/stash/packed-refs) can never be silently clobbered.
#[tokio::test]
async fn git_ff_divergent_submodule_ref_vetoes_whole_group() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-submodule-veto";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0; A and B pull C0.
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

    // A submodule's real gitdir lives at `<super>/.git/modules/dep/`. We model
    // its head ref as a raw-roamed pointer file — raw mode syncs all `.git/**`,
    // so a real functioning submodule is unnecessary: the fix is purely about
    // how `.git/modules/**` ref-class paths are CLASSIFIED by the FF veto.
    let dep_ref = ".git/modules/dep/refs/heads/main";

    // B seeds the submodule head ref → S0 and PUSHES it (B now tracks a clock
    // for it that matches the remote copy).
    std::fs::create_dir_all(b_repo.join(".git/modules/dep/refs/heads")).unwrap();
    std::fs::write(b_repo.join(dep_ref), format!("{}\n", "a".repeat(40))).unwrap();
    let b_seed_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B submodule-ref seed plan");
    assert!(
        b_seed_plan.actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Push { rel_path, .. } if rel_path == dep_ref
        )),
        "setup: the submodule head ref must roam as a raw push, got {:?}",
        b_seed_plan.actions
    );
    execute_plan(
        &b_seed_plan,
        &op,
        &b_repo,
        prefix,
        &mut b_state,
        "device-b",
        None,
        None,
    )
    .await
    .expect("B submodule-ref seed execute");
    b_state.flush().unwrap();

    // A commits C1 and pushes (remote head clock now carries a device-a tick;
    // the head conflict FF-reclassifies and dominates).
    commit_and_sync(
        &op,
        &a_repo,
        prefix,
        "device-a",
        &mut a_state,
        &blacklist,
        &cfg,
        "from_a.txt",
        b"a-c1\n",
        "C1 on A",
    )
    .await;
    let c1 = head_sha(&a_repo);

    // B fast-forwards main PAST C1 (a strict descendant, C2) with a concurrent
    // clock, and RE-POINTS the submodule head ref → a DIVERGENT value WITHOUT
    // re-syncing. Its tracked submodule-ref clock is unchanged, so the new
    // content vs the remote's S0 is an equal-clock DIVERGENT conflict — a
    // ref-class group member that groups under the OUTER repo root. A different
    // length guarantees the change is detected (never skipped by a same-size /
    // same-second quick check).
    git(
        &b_repo,
        &[
            "fetch",
            "--quiet",
            &a_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    git(&b_repo, &["reset", "-q", "--hard", &c1]);
    let _c2 = commit(&b_repo, "from_b.txt", b"b-c2\n", "C2 on B");
    std::fs::write(b_repo.join(dep_ref), format!("{}\n", "b".repeat(64))).unwrap();
    tick_tracked_vclock(&mut b_state, ".git/refs/heads/main", "device-b");

    let b_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B plan");

    let conflicts = git_conflicts(&b_plan);
    assert!(
        conflicts.iter().any(|c| c.ends_with(".git/refs/heads/main")),
        "BLOCKER-3: the outer head ref must STAY Conflict (group vetoed by the divergent submodule ref), got {conflicts:?}"
    );
    assert!(
        conflicts.iter().any(|c| c == dep_ref),
        "BLOCKER-3: the divergent submodule head ref must STAY Conflict, got {conflicts:?}"
    );
    // Neither the outer head nor the submodule ref may reclassify to push/pull —
    // the whole group is fail-closed.
    assert!(
        !git_ref_push(&b_plan) && !git_ref_pull(&b_plan),
        "BLOCKER-3: the outer head must not reclassify to push/pull"
    );
    assert!(
        !b_plan.actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Push { rel_path, .. } | ReconcileAction::Pull { rel_path, .. }
                if rel_path == dep_ref
        )),
        "BLOCKER-3: the divergent submodule ref must not be force-synced under group dominance"
    );

    drop(a_state);
    drop(s_state);
}

/// Detached-`.git/HEAD` variant of BLOCKER-3 (PR #513): a divergent, detached
/// top-level `.git/HEAD` (a raw commit SHA) sharing the group with an FF-ahead
/// branch ref must ALSO veto the whole group. `.git/HEAD` is ref-class but
/// `head_ref_for_git_path` never proves it (it matches only `.git/refs/heads/*`),
/// so the same fail-closed veto covers it for free.
#[tokio::test]
async fn git_ff_divergent_detached_head_vetoes_whole_group() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }

    let op = memory_operator();
    let prefix = "test/git-ff-detached-head-veto";
    let blacklist = raw_blacklist();
    let cfg = ff_config();

    // Seed S at C0; A and B pull.
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

    // A commits C1 and pushes → remote head advances to C1.
    commit_and_sync(
        &op,
        &a_repo,
        prefix,
        "device-a",
        &mut a_state,
        &blacklist,
        &cfg,
        "from_a.txt",
        b"a-c1\n",
        "C1 on A",
    )
    .await;
    let c1 = head_sha(&a_repo);

    // B fast-forwards main PAST C1 to C2 (strict descendant) with a concurrent
    // clock → the branch head ref is genuinely FF-ahead + reclassify-eligible.
    git(
        &b_repo,
        &[
            "fetch",
            "--quiet",
            &a_repo.join(".git").to_string_lossy(),
            &format!("{c1}:refs/remotes/peer/main"),
        ],
    );
    git(&b_repo, &["reset", "-q", "--hard", &c1]);
    let _c2 = commit(&b_repo, "from_b.txt", b"b-c2\n", "C2 on B");
    tick_tracked_vclock(&mut b_state, ".git/refs/heads/main", "device-b");

    // DETACH B's HEAD to a divergent raw SHA WITHOUT re-syncing. While on a
    // branch, commits never rewrite `.git/HEAD` (it stays `ref: refs/heads/main`
    // — a 21-byte symref), so its tracked clock still matches the remote copy.
    // Overwriting it with a 41-byte raw SHA makes it an equal-clock DIVERGENT
    // ref-class conflict; the different length guarantees detection.
    std::fs::write(b_repo.join(".git/HEAD"), format!("{}\n", "d".repeat(40))).unwrap();

    let b_plan = reconcile(
        &op, &b_repo, prefix, &b_state, "device-b", &blacklist, &cfg, None,
    )
    .await
    .expect("B plan");

    let conflicts = git_conflicts(&b_plan);
    assert!(
        conflicts.iter().any(|c| c.ends_with(".git/refs/heads/main")),
        "detached-HEAD: the FF-ahead branch ref must STAY Conflict (group vetoed by the divergent HEAD), got {conflicts:?}"
    );
    assert!(
        conflicts.iter().any(|c| c == ".git/HEAD"),
        "detached-HEAD: the divergent detached HEAD must STAY Conflict, got {conflicts:?}"
    );
    assert!(
        !git_ref_push(&b_plan) && !git_ref_pull(&b_plan),
        "detached-HEAD: the whole group is fail-closed — no branch-ref push/pull"
    );

    drop(a_state);
    drop(s_state);
}

/// TIN-2584 (canary shape): a head ref rewritten out-of-band on `honey` carries
/// a stale vclock that the remote (advanced by `neo`) STRICTLY DOMINATES. Before
/// the fix, `compare_clocks` reads `{neo:1}` vs `{neo:2}` as `RemoteNewer` and
/// the incoming pull path silently parks the divergence — `tcfs conflicts` stays
/// empty. The divergence tick must instead classify it as a `Conflict` that the
/// record arm surfaces, with ZERO writes to the diverged ref.
///
/// `raw_no_ff_config()` keeps the FF reclassifier OFF so the Conflict can ONLY
/// originate at the vclock layer (the divergence tick) — not a reclassification.
#[tokio::test]
async fn git_out_of_band_dominated_ref_records_conflict() {
    if !git_available() {
        eprintln!("git not available; skipping git_out_of_band_dominated_ref_records_conflict");
        return;
    }

    let op = memory_operator();
    let prefix = "test/tin2584-canary";
    let blacklist = raw_blacklist();
    let ff = ff_config();

    // Seed S at C0; raw-push the whole tree.
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

    // honey and neo both pull C0 (each adopts the remote head clock).
    let honey_tmp = TempDir::new().unwrap();
    let honey_repo = honey_tmp.path().join("repo");
    std::fs::create_dir_all(&honey_repo).unwrap();
    let mut honey_state = pull_into(
        &op,
        &honey_repo,
        prefix,
        "device-honey",
        &honey_tmp.path().join("honey.db"),
        &blacklist,
        &ff,
    )
    .await;

    let neo_tmp = TempDir::new().unwrap();
    let neo_repo = neo_tmp.path().join("repo");
    std::fs::create_dir_all(&neo_repo).unwrap();
    let mut neo_state = pull_into(
        &op,
        &neo_repo,
        prefix,
        "device-neo",
        &neo_tmp.path().join("neo.db"),
        &blacklist,
        &ff,
    )
    .await;

    // neo commits C1 and pushes (FF over C0) — the remote head clock advances to
    // strictly dominate honey's still-{...s:1} tracked clock.
    commit_and_sync(
        &op,
        &neo_repo,
        prefix,
        "device-neo",
        &mut neo_state,
        &blacklist,
        &ff,
        "from_neo.txt",
        b"neo-c1\n",
        "C1 on neo",
    )
    .await;

    // honey commits its OWN divergent sibling out-of-band (never resyncs), so
    // its tracked head clock stays dominated while the ref content diverges.
    commit(&honey_repo, "from_honey.txt", b"honey-c1\n", "C1 on honey");
    // Force the same-second, same-size (41-byte) ref rewrite the canary hit, so
    // classification must trust the content hash, not stat granularity.
    pin_mtime_to_cached_second(&honey_state, &honey_repo, ".git/refs/heads/main");

    let head_manifest_before = list_remote_index(&op, prefix)
        .await
        .expect("remote index before")
        .get(".git/refs/heads/main")
        .expect("remote head ref entry")
        .manifest_hash
        .clone();

    // honey reconciles with the FF reclassifier OFF: the only route to a Conflict
    // is the divergence tick.
    let no_ff = raw_no_ff_config();
    let honey_plan = reconcile(
        &op,
        &honey_repo,
        prefix,
        &honey_state,
        "device-honey",
        &blacklist,
        &no_ff,
        None,
    )
    .await
    .expect("honey reconcile plan");

    assert!(
        git_conflicts(&honey_plan)
            .iter()
            .any(|c| c.ends_with(".git/refs/heads/main")),
        "TIN-2584: the dominated out-of-band divergence must record a head-ref Conflict, got {:?}",
        git_conflicts(&honey_plan)
    );
    assert!(
        !git_ref_pull(&honey_plan),
        "TIN-2584: the divergence must NOT be classified as a silent RemoteNewer pull"
    );

    // Execute: the conflict is recorded and the remote head ref is untouched.
    let honey_exec = execute_plan(
        &honey_plan,
        &op,
        &honey_repo,
        prefix,
        &mut honey_state,
        "device-honey",
        None,
        None,
    )
    .await
    .expect("honey execute");
    honey_state.flush().unwrap();
    assert!(
        honey_exec.conflicts_recorded >= 1,
        "TIN-2584: the head-ref conflict must be recorded so keep-both can act"
    );

    let head_manifest_after = list_remote_index(&op, prefix)
        .await
        .expect("remote index after")
        .get(".git/refs/heads/main")
        .expect("remote head ref entry after")
        .manifest_hash
        .clone();
    assert_eq!(
        head_manifest_before, head_manifest_after,
        "TIN-2584: recording the conflict must not write the diverged head ref"
    );

    drop(s_state);
    drop(neo_state);
}
