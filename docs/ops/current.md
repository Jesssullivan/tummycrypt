# Current TCFS workstream

Source boundary last verified: **2026-08-22** against tummycrypt `origin/main`
`dfe8282a4d93af0ca1c495065ec82002352d4d86`. The live host rows retain their
older evidence boundaries; this source refresh claims no deployment or
ceremony.

Live `neo`/`honey` rows were re-measured **2026-07-29** via a read-only
bilateral probe (see the Fleet row and Live defect findings below); the
source boundary above is unchanged.

This is the living blocker list. Dated plans and evidence packets remain useful
history, but they do not override this page.

## Product posture

TCFS has crossed the mechanism threshold and has not crossed the daily-driver
product threshold.

| Surface | Proven now | Still open |
| --- | --- | --- |
| Git roam | One complete forward repo roam; automatic divergent keep-both without committed-work loss; PR #551 daemon-trusted conflict routing landed after the pre-freeze root-targeted run cleared the production `.git` loop | Residual production-root closure and the two-repo stop rule |
| Agent state | One bounded Claude project subtree on neo/honey; the reconcile units were live-verified on both hosts on 2026-07-29 (kickstarted, one clean cycle each: 11 entries synced, 0 conflicts) | Arbitrary sessions, Codex state, prompts, and cross-OS cwd mapping; the canary target itself has been dormant since 2026-04-25, so cache mtime cannot serve as a liveness signal — [TIN-3300](https://linear.app/tinyland/issue/TIN-3300) tracks retargeting and last-reconcile-age surfacing |
| Hydration | Linux FUSE lifecycle; bounded signed macOS FileProvider lifecycle | Plain-root parity, polished Finder first run, NFS/Windows/iOS parity |
| Home state | A few explicitly managed paths | Selective product enrollment for home/dotdir classes |
| Fleet | Honey runs `v0.12.17`; neo version coherence is now measured closed — `which -a tcfs` returns `/Users/jess/.local/bin/tcfs` then `~/.nix-profile/bin/tcfs`, both `v0.12.17`, and the daemon's gRPC status also reports `v0.12.17` (measured 2026-07-29) | The residual defect is provenance, not version skew: `/Users/jess/.local/bin/tcfs` is an unmanaged, hand-placed binary (dated Jul 26) shadowing the home-manager symlink, with nothing keeping it converged; sting remains `v0.12.16`; Bumble is the formal R6 host |
| Security | Stored content is encrypted; TOTP is enrolled on honey | Production S3 uses plaintext HTTP; headless sessions and invitation persistence are incomplete |
| Packaging | Tagged Nix release and several artifact lanes exist | Homebrew stale; Rocky RPM/FUSE and vendor acceptance unproven |
| Registered roots | Source supports deterministic `reconcile --root <id>` and exact-plan execution for explicitly reconcile-enabled bindings; TIN-3277/3278 regressions and Bulkload GitWorkspaceV2 decoding are covered in Rust | Installed eGreg pilot, host activation, and live convergence evidence |

## Closed and corrected

- G5-git-5 is closed by
  [PR #542](https://github.com/Jesssullivan/tummycrypt/pull/542). The proof is
  the automatic loser-side keep-both guard, not the operator
  `tcfs resolve --execute` path.
- TIN-2657 is fixed by
  [PR #545](https://github.com/Jesssullivan/tummycrypt/pull/545): the primary
  CLI and daemon state-cache path now converges on the canonical JSON file.
- TIN-2853's source seam landed through
  [PR #551](https://github.com/Jesssullivan/tummycrypt/pull/551) on 2026-07-18
  (merge commit `929bbf1`). This accepts the daemon-trusted conflict-only
  route; it is not evidence of a post-freeze live resolver or deployment.
- Neo version coherence is now **measured closed**: `which -a tcfs` returns
  `/Users/jess/.local/bin/tcfs` then `~/.nix-profile/bin/tcfs`, and both
  report `v0.12.17`; the daemon's gRPC status also reports `v0.12.17`
  (measured 2026-07-29). The residual defect is provenance, not version
  skew — `/Users/jess/.local/bin/tcfs` is an unmanaged, hand-placed binary
  (dated Jul 26) shadowing the home-manager symlink, and nothing keeps it
  converged with the managed build going forward.

Any document that still calls TIN-2657 open or describes G5-git-5 as awaiting
the divergent canary is historical.

## Production conflict gate

[TIN-2658](https://linear.app/tinyland/issue/TIN-2658/live-prod-repo-git-roam-tool-daemon-stuck-in-permanent-6-path-conflict)
is the active production resolver gate for `tinyland-tool-daemon`.

Current evidence:

- Neo and honey have the same Git commit and byte-identical tracked
  `README.md` and `AGENTS.md`.
- Before the TIN-2856 incident freeze, the source branch at `f508836`
  completed the root-targeted Git keep-both dry-run and execute on Honey.
- Two manually driven reconcile cycles cleared the 909+ cycle `.git` conflict
  loop.
- Deliberate user-content conflicts for `README.md` and `AGENTS.md` remain, as
  does the stale `roam-canary-wip` ref pair.
- PR #551/TIN-2853 has landed; no post-freeze live action was used to make that
  source claim.
- TIN-2856 freezes every further live resolver, enrollment/TOTP, deploy, and
  crypto ceremony. Source review, tests, and landing may continue without a
  new fleet claim.

The residual closeout is:

```text
wait for TIN-2856 live-work clearance
  → adjudicate README.md and AGENTS.md
  → handle the stale roam-canary-wip ref pair
  → git/content/state evidence
  → close TIN-2658
```

The full evidence boundary and root invariants are in
[`../PRODUCT.md`](../PRODUCT.md).

### Live defect findings (2026-07-29)

- [TIN-3277](https://linear.app/tinyland/issue/TIN-3277) — Home-manager
  writes to `secrets/*` and `devices.json` on neo bypass the vclock tick,
  producing permanent self-conflicts on those paths that the reconciler
  cannot push.
- [TIN-3299](https://linear.app/tinyland/issue/TIN-3299) — Honey shows a
  live hydration failure on `fp-proof-hMn6.txt`, 4,130 orphaned chunks, and
  stale `data`/`index` entries; a second, unaccounted worker daemon runs
  from `/etc/tcfsd/config.toml`, and plaintext HTTP was observed live.
- [TIN-3300](https://linear.app/tinyland/issue/TIN-3300) — The
  claude-projects reconcile units are healthy (live-verified on both hosts
  2026-07-29 after a false-positive "dead canary" reading), but the canary's
  sole target directory has been dormant since 2026-04-25, so the state
  cache cannot distinguish "nothing to sync" from "reconciler hung." Open
  work: retarget the canary to an actively-changing directory and surface
  last-reconcile age in status output.

## Strategy A queue

1. **Delivery guardrails.** Remove TCFS from every moving lab flake-update
   lane, accept only reviewed immutable source identities, and block fleet
   activation while the transitional downstream pin remains. This is a
   source-only safety change, not version convergence.
2. **Attended neo cleanup.** Capture paths and hashes for every effective TCFS
   candidate, quarantine the unmanaged `/Users/jess/.local/bin/tcfs` PATH
   shadow with an explicit restoration path, and prove interactive and agent
   shells select the managed binary. This is now a provenance hazard, not a
   version-skew hazard: the shadow binary matches the managed `v0.12.17`
   build today, but nothing enforces that convergence going forward.
3. **Canonical pin and delivery.** Pin lab to the signed canonical `v0.12.17`
   tag and peeled commit, then prove candidate, pre-activation, and
   post-activation invariants on honey, neo, and sting.
4. **TLS.** Move the credential-bearing SeaweedFS/S3 path from the current
   internal plaintext HTTP endpoint to an authenticated TLS hostname and enable
   `storage.enforce_tls`.
5. **Stable root lifecycle.** Render one eGreg binding with lifecycle policy
   `reconcile`, run the installed binary's dry plan, then execute only the
   emitted SHA-256. Git-workspace roots require a complete Bulkload
   GitWorkspaceV2 capture and use bundle/ref transport; raw `.git` admin paths
   are never transported.
6. **TIN-2658 residual closure.** After TIN-2856 clears live work, adjudicate
   the two user-content conflicts and stale ref pair, then capture final
   Git/content/state convergence evidence. Do not repeat the already completed
   pre-freeze dry-run/execute sequence merely to recreate evidence.
7. **Headless auth and enrollment.** Close
   [TIN-2653](https://linear.app/tinyland/issue/TIN-2653/tcfs-auth-session-token-unusable-over-headless-ssh-keychain-write-only)
   and prove persisted invitation/bootstrap state without an auth bypass.
8. **Two-repo stop rule.** Drive
   [TIN-2306](https://linear.app/tinyland/issue/TIN-2306/tcfs-stop-rule-clearance-enroll-2-3-small-clean-repos-drive-two)
   through both directions, unsync/rehydrate, divergence, restore, and a clean
   second cycle.
9. **Fleet coherence.** Bring sting to the selected release and root topology;
   leave Bumble as the tracker-defined formal third-host acceptance.
10. **Truth cleanup.** Keep the five-document product spine current:
   [docs/VISION.md](../VISION.md), [docs/PRODUCT.md](../PRODUCT.md),
   docs/ops/current.md (this document),
   [docs/platform-support.md](../platform-support.md), and
   [docs/release/evidence/README.md](../release/evidence/README.md). Stale
   vision PR [#543](https://github.com/Jesssullivan/tummycrypt/pull/543)
   closed unmerged; the vision landed via
   [#549](https://github.com/Jesssullivan/tummycrypt/pull/549)
   (commit `3e86016`).

## Gates that remain red

- Per-device-only crypto. A client that cannot unwrap content must fail closed,
  never surface ciphertext as a file.
- Installed linked-worktree recovery. The source adapter now consumes the
  Bulkload GitWorkspaceV2 logical topology and bundle/ref content, but eGreg
  activation and recovery evidence remain open.
- Broad `~/git`, dotdir, Documents, or home takeover.
- WebAuthn and unattended enrollment.
- NFS, Windows, and iOS product parity.
- Formal Rockies adoption and Rocky 10/FUSE packaging.

## Separate operator-security lane

[TIN-2521](https://linear.app/tinyland/issue/TIN-2521) PZM password rotation is
urgent but separate from Strategy A implementation. It requires the attended
TTY/SOPS ceremony and must not be folded into a filesystem rollout.

## Build boundary

Do not use `neo` for heavy local Rust, Nix, or Darwin builds. Use CI or the
fleet build substrate. PZM offload is tactical and only valid when the lab
directory-health and strict remote-builder verifier are green.

## Evidence boundary

- Evidence under `docs/release/evidence/` is immutable.
- The superseded 2026-07-06 operator checkpoint, including the PZM/TCC/SSD
  context and TIN-2584/2652 defect ledger, remains available at
  `git show 21f8df303596d1b9f6f90cc7953eb8f65f353ac3:docs/ops/current-workstream-truth-2026-07-06.md`.
- APFS-only benchmark packets are baseline evidence, not TCFS performance
  results.
- A source-only, dry-run-only, readiness-only, or package-build result must be
  labeled as such.
- The installed-runtime acceptance entry point is
  `scripts/installed-runtime-acceptance.sh`; retired Neo/Honey packet scripts
  are historical evidence, not current acceptance commands.
- No daily-driver, platform, or packaging claim is current unless this page or
  a newer named evidence packet promotes it.
