# Current TCFS workstream

Last verified: **2026-07-14** against tummycrypt `origin/main`
`21f8df303596d1b9f6f90cc7953eb8f65f353ac3`, live GitHub and Linear state,
and read-only `neo`/`honey`/`sting` fleet inspection.

This is the living blocker list. Dated plans and evidence packets remain useful
history, but they do not override this page.

## Product posture

TCFS has crossed the mechanism threshold and has not crossed the daily-driver
product threshold.

| Surface | Proven now | Still open |
| --- | --- | --- |
| Git roam | One complete forward repo roam; automatic divergent keep-both without committed-work loss | Authenticated root-targeted operator resolution and the two-repo stop rule |
| Agent state | One bounded Claude project subtree on neo/honey | Arbitrary sessions, Codex state, prompts, and cross-OS cwd mapping |
| Hydration | Linux FUSE lifecycle; bounded signed macOS FileProvider lifecycle | Plain-root parity, polished Finder first run, NFS/Windows/iOS parity |
| Home state | A few explicitly managed paths | Selective product enrollment for home/dotdir classes |
| Fleet | Honey runs `v0.12.17`; neo has a managed `v0.12.17` build | Neo's effective interactive PATH still selects `v0.12.12`; sting remains `v0.12.16`; Bumble is the formal R6 host |
| Security | Stored content is encrypted; TOTP is enrolled on honey | Production S3 uses plaintext HTTP; headless sessions and invitation persistence are incomplete |
| Packaging | Tagged Nix release and several artifact lanes exist | Homebrew stale; Rocky RPM/FUSE and vendor acceptance unproven |

## Closed and corrected

- G5-git-5 is closed by
  [PR #542](https://github.com/Jesssullivan/tummycrypt/pull/542). The proof is
  the automatic loser-side keep-both guard, not the operator
  `tcfs resolve --execute` path.
- TIN-2657 is fixed by
  [PR #545](https://github.com/Jesssullivan/tummycrypt/pull/545): the primary
  CLI and daemon state-cache path now converges on the canonical JSON file.
- Honey currently runs `v0.12.17`. Neo has the managed `v0.12.17` build,
  but its effective interactive PATH still selects `v0.12.12`; version
  coherence is therefore not closed.

Any document that still calls TIN-2657 open or describes G5-git-5 as awaiting
the divergent canary is historical.

## Production conflict gate

[TIN-2658](https://linear.app/tinyland/issue/TIN-2658/live-prod-repo-git-roam-tool-daemon-stuck-in-permanent-6-path-conflict)
is the active production resolver gate for `tinyland-tool-daemon`.

Current evidence:

- Neo and honey have the same Git commit and byte-identical tracked
  `README.md` and `AGENTS.md`.
- Honey has three untracked canary/scratch files.
- Reconcile repeatedly reports six conflicts.
- The isolated state caches retain eight entries, including stale branch/stash
  paths.
- `tcfs conflicts --state` can inspect an isolated cache.
- `tcfs resolve` still targets the daemon's primary cache and prefix.
- Honey's safe dry-run stopped at session authentication.

Honey already has a persisted 0600 TOTP credential file. Do not request a code
until root routing is deployed: authentication alone would still mutate the
wrong state context.

The safe closeout is:

```text
deploy root routing
  → freeze the named reconcile root
  → verify TOTP
  → root-targeted dry-run
  → execute
  → resolve/preserve ordinary-file conflicts
  → first reconcile
  → second clean reconcile
  → git/content/state evidence
  → close TIN-2658
```

The full ceremony and root invariants are in
[`../PRODUCT.md`](../PRODUCT.md).

## Strategy A queue

1. **Delivery guardrails.** Remove TCFS from every moving lab flake-update
   lane, accept only reviewed immutable source identities, and block fleet
   activation while the transitional downstream pin remains. This is a
   source-only safety change, not version convergence.
2. **Attended neo cleanup.** Capture paths and hashes for every effective TCFS
   candidate, quarantine the unmanaged `v0.12.12` PATH shadow with an explicit
   restoration path, and prove interactive and agent shells select the managed
   binary.
3. **Canonical pin and delivery.** Pin lab to the signed canonical `v0.12.17`
   tag and peeled commit, then prove candidate, pre-activation, and
   post-activation invariants on honey, neo, and sting.
4. **TLS.** Move the credential-bearing SeaweedFS/S3 path from the current
   internal plaintext HTTP endpoint to an authenticated TLS hostname and enable
   `storage.enforce_tls`.
5. **Stable root routing.** Land the daemon-owned root registry and authenticated
   resolver selection described in PRODUCT.
6. **TIN-2658 live closure.** Run the bounded ceremony above.
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
- Linked-worktree roaming. Gitfiles and shared worktree metadata need explicit
  reconstruction semantics.
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
- No daily-driver, platform, or packaging claim is current unless this page or
  a newer named evidence packet promotes it.
