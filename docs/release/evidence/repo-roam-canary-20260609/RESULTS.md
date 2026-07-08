# Repo-roam zero-diff canary — tinyland-tool-daemon — 2026-06-09

Deployed: B1 reconcile units (raw .git-as-files) on current tcfs 0.12.14, hm gen 386 (neo).

R0 source fingerprint (neo): 5b24b9f95065576e4f664141fe837e8e1d9e4586c2a5765720b34ed8b05ee0b3 (branch roam-canary-wip + staged AGENTS.md + unstaged README.md + untracked scratch + 1 stash, fsck=clean)
R1 neo PUSH: 126 pushed, 0 errors (raw .git incl objects/index/refs/logs+stash + working tree)
R2 honey PULL + compare: honey fingerprint = 5b24b9f9... IDENTICAL → dev-env-zero-diff=PASS (T13-Z), fsck clean both sides. Branch/staged/unstaged/untracked/stash all roamed exact.
R3 bidirectional (honey commit→neo): honey reconcile = 5 conflicts, 0 pushed → G5-git-5 concurrent-.git gate CONFIRMED expected-fail (AutoResolver not .git-aware). Forward roam clean; concurrent bidirectional needs .git-aware conflict resolution (future work) or unsync/rehydrate safe-handoff.

2026-07-06 supersession note: the fast-forward half of this expected-fail was
closed by #513 and live-proven in
`docs/release/evidence/bidirectional-ff-canary-20260705T225429Z/RESULTS.md`.
The divergent keep-both code path is merged through #534, but still needs fleet
deploy and the live divergent canary before this historical expected-fail can be
treated as fully green.

2026-07-08 update: the divergent (non-FF) half is now CLOSED. A two-host
neo ⇄ honey fleet canary live-proved the #534 loser-side no-loss guard — loser
head parked, both hosts converged to zero conflicts, no committed work lost
(`docs/release/evidence/divergent-keep-both-canary-20260707T071335Z/RESULTS.md`,
harness row G5-git-13). This historical expected-fail (G5-git-5) is now green
end-to-end. Two operator-VERB defects remain open (TIN-2653, TIN-2657) but do
not affect the automatic convergence proven here.
