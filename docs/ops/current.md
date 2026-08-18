# Current TCFS workstream

Source boundary last verified: **2026-08-17** against tummycrypt `origin/main`
`dfe8282a4d93af0ca1c495065ec82002352d4d86`. The live
`neo`/`honey`/`sting` rows retain their 2026-07-14 verification boundary; this
source refresh does not claim a new deployment, activation, or ceremony.

This is the living blocker list. Dated plans and evidence packets remain useful
history, but they do not override this page.

## Claim boundary

Keep four claim classes separate throughout this page:

- **Source** means reviewed bytes on tummycrypt `main`, or an explicitly named
  open PR. An open or draft PR is not landed source.
- **Runtime** means a dated observation from a named host. A managed generation,
  declared package, or process intent is not effective runtime by itself.
- **Proof** means a named, reproducible evidence packet or receipt that binds the
  source, runtime, operation, and result it claims.
- **Adoption** means the supported user workflow is activated and repeatable. A
  source merge, one attended recovery, or package smoke is not adoption.

## Current source lanes

- B0a is landed on `main`:
  [PR #563](https://github.com/Jesssullivan/tummycrypt/pull/563) completed the
  authorized immutable `roots list/status` seam. Reconcile support remains
  `NONE`; this is not a live lifecycle or deployment claim. The later TIN-1556
  lifecycle ADR is explicitly proposed design only.
- Draft [PR #572](https://github.com/Jesssullivan/tummycrypt/pull/572) is the
  current source-only CI-authority gate. Its exact signed head
  `3d4de77933f2234d491dce1be2ac9e46e6fb4b10` is based on this page's
  `dfe8282a` source boundary and is runtime-inert. Natural CI still exposes the
  missing Docker Compose v2 image capability (TIN-3798), SIGKILLs in the
  `rust-docs` derivation in CI/Windows and the `tcfs-tui` derivation in Nix CI
  (TIN-545), and held native Darwin authority. The
  `cargo-package-deps-0.12.17.drv` derivation completed successfully and must
  not be named as the terminal failure. TIN-3800's root-alias repair is
  source-reviewed on that head; it does not clear those gates.
- The source stack is serial: #572 CI authority, then #565 product, fresh signed
  replacements for the unsigned #576 and #577 heads, then #567 and #568. A
  clean child diff does not permit bypassing or reordering that chain.
- The `v0.12.18` release exception is ratified in scope and the tag cut is
  deferred. It is a bounded future rotation executor, not a fleet baseline.

## Adjacent evidence that is not TCFS adoption

- Bulkload `main` at `1515b98512c4c5dc7f205a15020b9cdc781c1195`
  contains landed source through PR #9 (PR #9 entered through the #8 branch)
  for typed Codex auth/SQLite capture, atomic auth replacement, offline
  planning, and a read-only verifier oracle. Those bytes and tests are scoped
  source evidence, not a named runtime receipt. SQLite composition,
  publication, installation, combined apply, and an executed session-union
  claim remain held; none of it proves TCFS roaming.
- A separate July 2026 attended Bulkload canary proved bounded authentication
  and fresh native-thread persistence, lookup, and dialog continuity. It did
  not prove tmux seat adoption, a full Sting-side work item, or TCFS transport.
  The 2026-08-16 Sting continuity incident remains in off-host evidence
  collection and adds no TCFS or agent-roaming adoption proof.
- Neo Home Manager generation 455 is operator-reported, but no dated local
  observation or accepted source/activation/rollback receipt binds it. Treat
  it as an unverified operator report, not accepted runtime or proof that TCFS,
  agent tooling, or any dependent surface was adopted; never backfill it as an
  authority.
- At prompts-enqueue `dda2ffcd9350913a6c9329f580a81c282c60eccf`,
  prompt 01 is `ready`; prompts 17, 18, and 47 are `draft`. Only prompt 01 is
  dispatchable, and its first unit must follow this page's earliest legal gate.
  Draft prose cannot promote agent roaming, per-device crypto, or Rockies
  adoption.

## Product posture

TCFS has crossed the mechanism threshold and has not crossed the daily-driver
product threshold.

| Surface | Proven now | Still open |
| --- | --- | --- |
| Git roam | One complete forward repo roam; automatic divergent keep-both without committed-work loss; PR #551 daemon-trusted conflict routing landed after the pre-freeze root-targeted run cleared the production `.git` loop | Residual production-root closure and the two-repo stop rule |
| Agent state | One bounded Claude project subtree on neo/honey | Arbitrary sessions, Codex state, prompts, and cross-OS cwd mapping |
| Root lifecycle | B0a authorized immutable `roots list/status` source is landed | Reconcile-by-root, adopt/remove, Lab rendering, lifecycle proof, and deployment |
| Hydration | Linux FUSE lifecycle; bounded signed macOS FileProvider lifecycle | Plain-root parity, polished Finder first run, NFS/Windows/iOS parity |
| Home state | A few explicitly managed paths | Selective product enrollment for home/dotdir classes |
| Fleet | At the 2026-07-14 runtime boundary, Honey ran `v0.12.17` and neo had a managed `v0.12.17` build | Neo's effective interactive PATH selected `v0.12.12`; sting remained `v0.12.16`; generation 455 is operator-reported, unverified, and unreceipted; Bumble is the formal R6 host |
| Security | Stored content is encrypted; the signed device registry was exercised; TOTP is enrolled on honey; the fleet remains in `master` wrap mode | Production S3 uses plaintext HTTP; headless sessions and invitation persistence are incomplete; no dual canary or per-device cutover is accepted; direct/uniffi clients cannot read per-device-only manifests |
| Packaging | Tagged Nix release and several artifact lanes exist | `v0.12.18` cut deferred; Homebrew stale; Rocky RPM/FUSE and vendor acceptance unproven |

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
- TIN-2863/B0a landed through PR #563. It accepts authorized immutable root
  inventory/status source only; broader root lifecycle and reconcile remain
  red.
- At the retained 2026-07-14 runtime boundary, Honey ran `v0.12.17`. Neo had
  the managed `v0.12.17` build, but its effective interactive PATH selected
  `v0.12.12`; version coherence is therefore not closed.

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
- TIN-2856 containment and harness work is recorded as accepted, but its
  credential-rotation ceremony has not executed. Parent TIN-2801 and
  `LAB_DEPLOY_FREEZE` still freeze live resolver, enrollment/TOTP, deploy, and
  crypto ceremonies. Source review, tests, and landing may continue without a
  new fleet claim.

The residual closeout is:

```text
wait for TIN-2801 / LAB_DEPLOY_FREEZE live-work clearance
  → satisfy the unexecuted TIN-2856 ceremony prerequisites where applicable
  → adjudicate README.md and AGENTS.md
  → handle the stale roam-canary-wip ref pair
  → git/content/state evidence
  → close TIN-2658
```

The full evidence boundary and root invariants are in
[`../PRODUCT.md`](../PRODUCT.md).

## Strategy A queue

1. **CI and delivery authority.** Close #572's TIN-3798, TIN-545, and native
   Darwin gates and land the reviewed source authority before advancing the
   serial product stack. Keep TCFS out of moving lab flake-update lanes, accept
   only reviewed immutable source identities, and block fleet activation while
   the transitional downstream pin remains. This is source-only safety, not
   version convergence.
2. **Attended neo cleanup.** Capture paths and hashes for every effective TCFS
   candidate, preserve generation 455 as unreceipted without retroactive
   backfill, use a future prospective generation for an exact
   activation/rollback receipt, quarantine the unmanaged `v0.12.12` PATH
   shadow with an explicit restoration path, and prove interactive and agent
   shells select the managed binary.
3. **Canonical pin and delivery.** Pin lab to the signed canonical `v0.12.17`
   tag and peeled commit, then prove candidate, pre-activation, and
   post-activation invariants on honey, neo, and sting.
4. **TLS.** Move the credential-bearing SeaweedFS/S3 path from the current
   internal plaintext HTTP endpoint to an authenticated TLS hostname and enable
   `storage.enforce_tls`.
5. **Stable root identity.** Keep landed PR #551's conflict-only route and
   landed TIN-2863/B0a authorized V1 `roots list/status` seam unchanged. B0a
   reports immutable persisted state and reconcile support `NONE`; #565 and the
   broader lifecycle remain source-review work, not MCP, mutation, rollout, or
   adoption authority.
6. **TIN-2658 residual closure.** After TIN-2801 and `LAB_DEPLOY_FREEZE` clear
   live work, adjudicate the two user-content conflicts and stale ref pair,
   then capture final Git/content/state convergence evidence. Do not repeat the
   already completed pre-freeze dry-run/execute sequence merely to recreate
   evidence.
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

- Per-device-only crypto. The live fleet remains in `master`; the dual canary
  has not run, rotation remains gated, and direct/uniffi clients cannot read
  per-device-only manifests. A client that cannot unwrap content must fail
  closed, never surface ciphertext as a file.
- Root adoption/removal, daemon-owned reconcile, subscriptions, and lifecycle
  recovery beyond B0a's immutable status seam.
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
