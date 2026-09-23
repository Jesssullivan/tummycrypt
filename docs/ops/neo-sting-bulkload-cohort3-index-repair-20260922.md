# 2026-09-22 — neo→sting migration cohort 3: missing-index repair (R-N39, TIN-3692)

Session note per R-N13. Operator posts the Linear comment; this is the durable working record.

## What ran
- Inventory `triples-v2.tsv` classes `repo-noindex` (76) + `wt-noindex` (46) = 122 rows; 2 held (GloriousFlywheel-*); 120 re-verified on both hosts.
- Dropped 9 destinations (10 rows): 5 HEAD sha differs neo≠sting (Fuzzy, blahaj/.worktrees/disarm-gf-worker-host-writer-20260824, darkmap.tinyland.dev, gdrive-mounts, gftb-site, jesssullivan-infra.worktrees/cull-20260810), 2 source missing on neo (account-controller.worktrees/tin-3105-hermetic-envtest, bulkload.worktrees/boundary-pass1-20260824), 1 source has no index on neo (demo). 37 rows were cross-product duplicates of the same destination (Scruff.jl / keepassxc / jesssullivan-infra / XoxdWM / acuity-middleware families) → 73 plan items.
- neo: `estate-add-batch` + `estate-capture` (JOBS=2, private state + corpus on /Volumes/TinylandState, 0700): captured 66, refused 7 `GIT_INVENTORY_MALFORMED` (8311-was-110-firmware-builder, Scruff.jl-tinyland-wayfinding, Scruff.jl-upstream-ci-devshell, caldera, cmux, dollhouse-farm, k8sy-windows). 19m12s, corpus 5.6G (66 item bundles + 4 shared bases, 12 delta bundles).
- sting: `pull` 152 corpus files, 5 864 514 759 bytes, 7m55s; sha256-identical to neo. One pull refusal (`plan.receipt` SOURCE_CHANGED_AFTER_SNAPSHOT — my own receipt appended mid-pull; not corpus).
- sting: `git-repair-missing-index BUNDLE WORKTREE neo-20260922-c3 NEW_RECEIPT` pilot 5 then remaining 68, sequential: repaired 63, refused 3 `GIT_INVENTORY_MALFORMED` (jesssullivan-infra, jesssullivan-infra-dsa-dns, jesssullivan-infra-wt/gdrive — `git bundle verify` reports prerequisite commits absent from sting's jesssullivan-infra repository; no carry refs written, index still absent), skipped 7 (no bundle; capture refused).

## Non-clobber proof
- 36 non-cohort worktrees of the same 63 repositories: `.git/index` (dev,ino,mtime) + HEAD bytes identical before/after.
- 73 cohort worktrees: refs/heads sha256 of repository, HEAD, branch identical before/after; only delta is index absent→present on the 63 repaired; `git status` exits 0 on all.

## Receipts
- sting: `/srv/fast-local/jess/bulkload/receipts/{pull,repair}-cohort3-20260922.{log,before,pilot,after}`; verb receipts `/srv/fast-local/jess/bulkload/cohort3-20260922-repair/NNN-<name>-<hash>/` (captured.index, original-administration.postcard); working state `/srv/fast-local/jess/bulkload/state-cohort3-20260922/`.
- neo: `/Volumes/TinylandState/tinyland-state/bulkload-cohort3-20260922/{plan.postcard,plan.receipt,capture.log,corpus/,receipts-from-sting/}`; private state `/Volumes/TinylandState/tinyland-state/bulkload-cohort3-private/`; pull src state `/Users/jess/state/bulkload-cohort3-20260922-srcstate/`.

## Learned
- For a linked worktree REPOSITORY is the worktree path itself (verb derives admin dir via `rev-parse --absolute-git-dir`); NEW_RECEIPT must be a not-yet-existing dir on the same filesystem as the index (hard link) and outside the worktree/admin.
- Delta (thin) bundles repair fine when sting already holds the prerequisite commits (HEAD equal); they refuse `GIT_INVENTORY_MALFORMED` when sting's repository lacks other prerequisite commits — re-run once the drift-tolerant custody PR lands or after refs are imported.
- Cohort 2a/2b applies (R-N36/R-N38) ran concurrently on the same hosts; per-item pre/post digests were captured tightly around each verb call and show no interference.
