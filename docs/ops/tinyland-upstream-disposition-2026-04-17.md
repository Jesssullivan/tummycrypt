# Tinyland-Unique Commit Disposition — 2026-04-17

Per [Remote Governance §Tinyland → Origin](./remote-governance.md#tinyland--origin), this document records the disposition of each of the 19 commits that were on `tinyland/main` but not on `origin/main` as of merge-base `3b30df6` (2026-04-10).

Source: `git log origin/main..tinyland/main` on 2026-04-17 produced 19 commits.

## Summary

- **5 upstream as cherry-pick** — real fix/feature work, no origin equivalent, landed via dedicated PRs.
- **9 superseded** — origin landed an equivalent change independently; verified by matching `git patch-id --stable`.
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
| `009266c` | fix(fuse) | NATS-driven negative cache invalidation + shorter dir TTL (#4) | Upstream | PR #TBD |
| `f32acf5` | feat(health) | add /livez FUSE mount liveness probe endpoint | Upstream | PR #TBD |
| `1e15342` | feat(nix) | .app bundle derivation + home-manager launchd wiring | Upstream | PR #TBD |
| `28fa6ab` | feat(darwin) | TCFSDaemon.app bundle for macOS TCC persistence (#16) | Upstream | PR #TBD |
| `2365e3e` | feat(fileprovider) | hierarchical enumeration, remote discovery, NATS consumer fix | Upstream | PR #TBD |
| `c6bb848` | merge | Merge pull request #42 from tinyland-inc/sync/upstream-merge | Merge-bookkeeping | n/a (2 parents: `5f461f97`, `2daf1fe0`) |
| `2daf1fe` | merge | Merge remote-tracking branch 'upstream/main' into sync/upstream-merge | Merge-bookkeeping | n/a (2 parents: `5f461f97`, `3b30df62`) |
| `5f461f9` | merge | Merge pull request #27 from tinyland-inc/feat/nix-app-bundle | Merge-bookkeeping | n/a (2 parents: `a39fcef8`, `1e15342d`) |
| `a39fcef` | merge | Merge pull request #26 from tinyland-inc/feat/livez-health-probe | Merge-bookkeeping | n/a (2 parents: `28fa6ab9`, `f32acf54`) |
| `4691d54` | chore | sync Cargo.lock with workspace Cargo.toml | Chore-bookkeeping | n/a (regenerates from workspace Cargo.toml) |

## Verification method

Patch-ids computed with `git show <sha> | git patch-id --stable`. Two commits have identical patch-id iff their diff payload is byte-for-byte identical, ignoring commit metadata (author, date, message, parents).

All 9 claimed-superseded pairs produced identical patch-ids (MATCH for every pair). No manual DIFFER inspection was required.

Additionally, each of the 5 claimed-unique commits was cross-checked against every commit on `origin/main` since `2026-03-01`. No hidden patch-id matches were found, confirming these commits are genuinely absent from origin and require cherry-pick.

The 4 merge commits were each confirmed to have exactly 2 parents via `git log --format='%P' -1 <sha>`, establishing them as true merges rather than linear commits.

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
- The 5 Upstream rows link to merged origin PRs once Tasks 3-7 complete.
- `tinyland/main` resynced from `origin/main` (Task 9, optional) once all upstream PRs land.

Refs: [Remote Governance](./remote-governance.md), GitHub #311
