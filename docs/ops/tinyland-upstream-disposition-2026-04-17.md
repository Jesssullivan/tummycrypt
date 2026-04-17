# Tinyland-Unique Commit Disposition — 2026-04-17

Per [Remote Governance §Tinyland → Origin](./remote-governance.md#tinyland--origin), this document records the disposition of each of the 19 commits that were on `tinyland/main` but not on `origin/main` as of merge-base `3b30df6` (2026-04-10).

Source: `git log origin/main..tinyland/main` on 2026-04-17 produced 19 commits.

## Summary

- **0 upstream as cherry-pick** — after full verification (see Verification method §2026-04-17 correction below), every candidate was found already on `origin/main`. No cherry-picks required.
- **14 superseded** — origin landed equivalent changes via two paths:
  - 9 commits superseded by dedicated origin PRs (verified by matching whole-commit `git patch-id --stable`).
  - 5 commits bundled into origin's PR #176 mega-bundle `e4596d6` (verified by path coverage + feature-level spot checks). Whole-commit patch-id did not match for these because PR #176 union'd multiple tinyland branches into one commit.
- **4 merge-bookkeeping** — tinyland-side merge commits; fall out naturally once feature commits land on origin.
- **1 chore-bookkeeping** — Cargo.lock regeneration; subsumed by workspace changes.

## Per-commit disposition

| Tinyland SHA | Type | Subject | Disposition | Upstream PR / origin SHA |
|--------------|------|---------|-------------|--------------------------|
| `7ba5277` | fix(sync) | eliminate panics — unwrap, bounds check, parse validation (#182) | Superseded | `aa2eac1a240ef2a60f55d59ce2f98bfd742d192a` (patch-id match) |
| `4b49aa2` | fix(vfs) | OOM prevention, RwLock for concurrent reads, bounded negative cache (#188) | Superseded | `283a0f815dfa209c6970048a71530bc0f054d9ca` (patch-id match) |
| `e5d957a` | fix(vfs) | use read() instead of lock() for RwLock fsync | Superseded | `4782f883298f6a837434d985079ff20e051c2722` (patch-id match) |
| `8580e71` | fix(fuse) | implement flush via VFS fsync + improve error-to-errno mapping (#187) | Superseded | `bc48a3e9816df520417cd3fd2a88b17ec9f3d239` (patch-id match) |
| `98ff12d` | fix(daemon) | race guards for FileDeleted, deferred vclock merge, active-file protection (#184) | Superseded | `5c679e2c0043c5d8f5c96b744fb4e006f0316c2f` (patch-id match) |
| `25be212` | fix(nats) | sync_always documentation + stream health verification (#181) | Superseded | `3e1342ea5108aeb3f482c81ed791f5b2aede623e` (patch-id match) |
| `1198549` | feat(sync) | selective sync policies + D-Bus gRPC backend (#192) | Superseded | `aecac188659068f7bf78feb136a954f5250837a1` (patch-id match) |
| `3e131b8` | feat(sync) | sync trash + bandwidth throttling (#193) | Superseded | `d9379ced4aa233b9ef83fa94cfd16f39dd201151` (patch-id match) |
| `bd55223` | feat | unsync dehydration + auto-unsync with disk pressure (#191) | Superseded | `0632903f3ed19aef1661a1ede48796c7c523a13b` (patch-id match) |
| `009266c` | fix(fuse) | NATS-driven negative cache invalidation + shorter dir TTL (#4) | Superseded | `e4596d672092a3b1761e64ca324cbb4cfc9d07be` (bundled in PR #176; `negative_ttl_secs` + invalidate-negative-cache plumbing present on origin `tcfs-fuse/src/driver.rs:534` at shifted line offsets) |
| `f32acf5` | feat(health) | add /livez FUSE mount liveness probe endpoint | Superseded | `e4596d672092a3b1761e64ca324cbb4cfc9d07be` (bundled in PR #176; `livez_handler` present on origin `tcfsd/src/metrics.rs:158` with +6L unwrap-hardening added after) |
| `1e15342` | feat(nix) | .app bundle derivation + home-manager launchd wiring | Superseded | `e4596d672092a3b1761e64ca324cbb4cfc9d07be` (bundled in PR #176; `TCFSDaemon.app` installPhase present on origin `flake.nix:139-143`; `nix/modules/tcfs-user.nix` byte-identical to origin HEAD) |
| `28fa6ab` | feat(darwin) | TCFSDaemon.app bundle for macOS TCC persistence (#16) | Superseded | `e4596d672092a3b1761e64ca324cbb4cfc9d07be` (bundled in PR #176; subset patch-id `f41f771b…` on `swift/daemon/` matches exactly) |
| `2365e3e` | feat(fileprovider) | hierarchical enumeration, remote discovery, NATS consumer fix | Superseded | `e4596d672092a3b1761e64ca324cbb4cfc9d07be` (bundled in PR #176; PR body explicitly lists "feat(fileprovider): hierarchical enumeration, remote discovery, NATS consumer fix"; enumerator present on origin `FileProviderExtension.swift` at matching line numbers 189/438) |
| `c6bb848` | merge | Merge pull request #42 from tinyland-inc/sync/upstream-merge | Merge-bookkeeping | n/a (2 parents: `5f461f97`, `2daf1fe0`) |
| `2daf1fe` | merge | Merge remote-tracking branch 'upstream/main' into sync/upstream-merge | Merge-bookkeeping | n/a (2 parents: `5f461f97`, `3b30df62`) |
| `5f461f9` | merge | Merge pull request #27 from tinyland-inc/feat/nix-app-bundle | Merge-bookkeeping | n/a (2 parents: `a39fcef8`, `1e15342d`) |
| `a39fcef` | merge | Merge pull request #26 from tinyland-inc/feat/livez-health-probe | Merge-bookkeeping | n/a (2 parents: `28fa6ab9`, `f32acf54`) |
| `4691d54` | chore | sync Cargo.lock with workspace Cargo.toml | Chore-bookkeeping | n/a (regenerates from workspace Cargo.toml) |

## Verification method

### Phase 1 — whole-commit patch-id (initial pass)

Patch-ids computed with `git show <sha> | git patch-id --stable`. Two commits have identical patch-id iff their diff payload is byte-for-byte identical, ignoring commit metadata (author, date, message, parents).

All 9 claimed-superseded pairs (first 9 rows) produced identical whole-commit patch-ids — MATCH for every pair. These are individually-superseded by dedicated origin commits.

The 4 merge commits were each confirmed to have exactly 2 parents via `git log --format='%P' -1 <sha>`, establishing them as true merges rather than linear commits.

### Phase 2 — subset patch-id + path coverage + feature-level spot check (correction pass)

The initial pass also cross-checked the 5 claimed-unique commits against origin/main with whole-commit patch-id and found no hidden matches, concluding they were genuinely unique. **This was wrong.** A Task 3 cherry-pick attempt on `28fa6ab` surfaced the root cause: origin's PR #176 `e4596d6` ("fix(sync): P0 sync engine") is a 21-file mega-bundle that absorbed 7+ tinyland feature branches at once. Its whole-commit diff does not byte-match any single tinyland commit, but each tinyland commit's change is present inside the bundle.

Three-signal verification was then applied to all 5 "upstream candidates":

1. **Subset patch-id** (Check A) — compute `git show <sha> -- <touched paths> | git patch-id --stable` for each candidate, scan origin since merge-base. Necessary but not sufficient: origin's context lines within `e4596d6` accumulate additional surrounding code from other bundled branches, so subset patch-id can still miss.
2. **Content-state comparison** (Check B) — compare candidate's `<sha>:file` vs `origin/main:file` for each touched path. Quantifies drift; reveals byte-matches when origin has accumulated no further changes.
3. **Feature-level grep on marquee functionality** — the decisive signal when Checks A and B disagree. Searches origin/main HEAD for the candidate's signature symbols (function names, comment markers, installPhase blocks).

All 5 rows previously marked `Upstream` were reclassified `Superseded` under this protocol. Evidence is recorded inline in each row's origin SHA column.

### Path coverage of `e4596d6`

`e4596d6` touches 21 files. The touched-file sets of the 5 reclassified commits are all subsets of those 21 files:

- `009266c` (5 paths) — 5/5 inside `e4596d6`
- `f32acf5` (2 paths) — 2/2 inside `e4596d6`
- `1e15342` (2 paths) — 2/2 inside `e4596d6`
- `28fa6ab` (3 paths) — 3/3 inside `e4596d6`
- `2365e3e` (5 paths) — 5/5 inside `e4596d6`

100% path coverage + feature-level presence = Superseded (bundled-with-drift).

### Step 1 raw output

```
7ba5277    vs aa2eac1a    MATCH
  tl:   c559502523b964ef529c389b0fd12080ea5dd6cf
  orig: c559502523b964ef529c389b0fd12080ea5dd6cf
4b49aa2    vs 283a0f81    MATCH
  tl:   644a6d26f25db9040a87909473b2ab6e338fd719
  orig: 644a6d26f25db9040a87909473b2ab6e338fd719
e5d957a    vs 4782f883    MATCH
  tl:   25fc575413ee5c513acd2eeb24d2daa81c5c8de7
  orig: 25fc575413ee5c513acd2eeb24d2daa81c5c8de7
8580e71    vs bc48a3e9    MATCH
  tl:   08e02aaff8752bd3096ba3bffa83f1fd91baffad
  orig: 08e02aaff8752bd3096ba3bffa83f1fd91baffad
98ff12d    vs 5c679e2c    MATCH
  tl:   d5825b31ba05db59a9520e7d1b6191c8b94ad546
  orig: d5825b31ba05db59a9520e7d1b6191c8b94ad546
25be212    vs 3e1342ea    MATCH
  tl:   62e18090fea0742a9305f02830fd208ba0ea8b44
  orig: 62e18090fea0742a9305f02830fd208ba0ea8b44
1198549    vs aecac188    MATCH
  tl:   767b6baa321bb524a033987a510a72dafe6027a5
  orig: 767b6baa321bb524a033987a510a72dafe6027a5
3e131b8    vs d9379ced    MATCH
  tl:   52ef95f009f70676124798dc53e6c82fe69530d6
  orig: 52ef95f009f70676124798dc53e6c82fe69530d6
bd55223    vs 0632903f    MATCH
  tl:   42ba27324add2a011639062100d52102b64e7260
  orig: 42ba27324add2a011639062100d52102b64e7260
```

## Acceptance

- All 19 commits have a recorded disposition above.
- **Zero upstream cherry-picks required.** Every commit is either superseded by origin, a merge bookkeeping entry, or a Cargo.lock chore entry.
- `tinyland/main` may be resynced from `origin/main` (governance sync policy) at any time since no tinyland-unique feature commits remain unrepresented on origin.

## Revision history

- **2026-04-17 (initial)** — 5 Upstream / 9 Superseded / 4 Merge-bookkeeping / 1 Chore-bookkeeping, based on whole-commit patch-id.
- **2026-04-17 (correction)** — 0 Upstream / 14 Superseded / 4 Merge-bookkeeping / 1 Chore-bookkeeping, after three-signal verification (subset patch-id + content comparison + feature-level spot check) surfaced that PR #176 `e4596d6` is a mega-bundle that absorbed all 5 claimed-unique commits. See §Verification method Phase 2.

Refs: [Remote Governance](./remote-governance.md), GitHub #311
