# Bidirectional FF handoff canary — LIVE — 2026-07-05T22:54:29Z

**Closes the FF half of G5-git-5 (TIN-1620 / TIN-1908) LIVE.** The R3 bidirectional
`.git`-conflict handoff — work on one host, resume on the other — now CONVERGES
via #513's fast-forward resolver where the 2026-06-09 canary stalled at 5 conflicts.

## Fleet
- neo: macOS, tcfs 0.12.16 (hm gen 417), reconcile invokes tcfs-cli-0.12.16 (lab pin 19ae294 ⊇ #513/c40f075)
- honey: Rocky 10, tcfs 0.12.16
- Both hosts on #513 FF resolution; raw `.git`-as-files (sync_git_dirs=true, git_sync_mode=raw); disposable prefix `git-roam/bidi-ff-*`
- PZM aarch64-darwin builder evidence from this day is superseded by later
  2026-07-06 lab probes: Finder/APFS can look healthy while SSH directory
  health still times out and System Policy/FDA denies shell/Nix-store paths.
  Do not use this packet as current PZM builder acceptance.

## Result — both directions converge, zero conflicts
| Step | Action | Outcome |
|------|--------|---------|
| 1 | neo seeds baseline `2382e64`, pushes | 136 pushed, 0 conflicts |
| 2 | honey pulls baseline | 136 pulled → honey at `2382e64`, fsck clean |
| 3 | **honey commits `a7c2da9`** (work-on-honey), pushes | 6 pushed |
| 4 | **neo reconciles** | **9 pulled, 0 conflicts → neo FF to `a7c2da9`**; HONEY_WORK.txt present; fsck clean; 0 dirty |
| 5 | **neo commits `b228674`** (reverse), pushes | 9 pushed |
| 6 | **honey reconciles** | **9 pulled, 0 conflicts → honey FF to `b228674`**; NEO_WORK.txt present; fsck clean; 0 dirty |

## Significance
The 2026-06-09 canary proved FORWARD dev-env zero-diff roam but the reverse
(honey→neo) stalled: "5 conflicts, 0 pushed" — the G5-git-5 gap. #513 (merged
c40f075, shipped in v0.12.16) reclassifies a concurrent `.git` ref conflict by
git ancestry: an ancestor→descendant relationship is a fast-forward, resolved by
converging to the descendant instead of recording a conflict. Confirmed LIVE
here in BOTH directions on the deployed fleet: **"the machine doesn't matter" —
commit on either host, resume on the other, byte-exact + fsck-clean, zero manual
intervention.**

Remaining: divergent (non-FF) keep-both code is merged through PR-4 (#534), but
the post-PR-4 fleet deploy and divergent live canary are still pending. This
packet remains the FF acceptance (harness G5-git-9/-11 class), not the divergent
T10/T11 proof.
