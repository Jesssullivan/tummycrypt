# Repo-roam no-crutch canary — v0.12.15 — 2026-07-02T05:23:34Z

Forward dev-env zero-diff roam neo→honey on **deployed tcfs 0.12.15** (both hosts
switched from 0.12.14 this session), raw `.git`-as-files, proving the merged
restore-fidelity fixes (#508 mtime-preservation + tracked-symlink converge) live —
**without** the manual `git update-index --refresh` mitigation the 2026-06-09
canary depended on.

## Fleet
- neo: macOS, tcfs 0.12.15 (hm gen 407), FileProvider daemon, authoritative pusher
- honey: Rocky 10, tcfs 0.12.15, subordinate pull
- canary repo: tinyland-tool-daemon (small clean scaffold), disposable prefix `git-roam/nocrutch-20260702T052148Z`

## Result
| Step | Outcome |
|------|---------|
| R0 neo fingerprint | `2277a886…`; branch nocrutch-wip + staged AGENTS.md + unstaged README.md + untracked NOCRUTCH_SCRATCH.txt + 1 stash; fsck clean |
| R1 neo push | 135 pushed, 0 conflicts, 0 errors (fresh prefix, clean bulk) |
| R2 honey pull + compare (NO refresh) | 135 pulled, 0 conflicts; honey fingerprint `2277a886…` **IDENTICAL**; `git status` correct with **no update-index --refresh**; fsck clean both sides → **dev-env-zero-diff=pass** |

## Significance
The 2026-06-09 canary (0.12.14) needed a manual `git update-index --refresh`
post-pull because restore stamped fresh mtimes → the byte-restored `.git/index`
saw every file as stat-dirty. #508 (merged, now deployed as 0.12.15) preserves
the source mtime on restore, so honey's index stat-cache matches immediately and
`git status` is clean with zero manual intervention. This closes the mtime
residual called out in the repo-roam test plan §5.

Bidirectional concurrent-edit convergence remains the G5-git-5 gate (PR #513,
in adversarial review).
