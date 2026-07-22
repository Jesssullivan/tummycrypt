# TIN-2856 Credential-Rotation Ceremony — Runbook (2026-07-16)

**Status:** DESIGNED-ONLY (ceremony not yet executed; reconciled 2026-07-22
against the landed GitHub-custody and prompt-pulse controls).
**Owner:** operator (Jess). Agents may stage `[PREP-SAFE-NOW]` steps; every
`[SECRET-GATE]` step is operator-executed, attended, and interviewed before it runs.
**Incident:** TIN-2856 (TCFS encryption passphrase exposed to agent execution logs via
`bash -x` inheriting `TCFS_ENCRYPTION_KEY_FILE`, 2026-07-14), child of TIN-2801
(PR lab#721 shell-expanded env leak). Related gates: generic GitHub runtime
convergence remains separate under TIN-2801; stale LaunchAgent exposure remains
TIN-1954 scope; and the new passphrase inherits the QR-invite plaintext caveat until
TIN-1424 closes.
**Freeze:** `LAB_DEPLOY_FREEZE` + nix-deploy gate (lab#825) stay in force until
TIN-2801 closes. This ceremony closes only the TCFS child gate; it does not lift
the fleet freeze while any TIN-2801 provider-ledger row remains open. TIN-2856
blocks TIN-2306, TIN-2658 close-out, TIN-1903/1904, TIN-1620, and TIN-1546.

The original Phase 1 credential design was superseded after this runbook landed.
Lab PR #876 deliberately separated the personal `read:packages` credential from
generic GitHub API, Nix-fetch, MCP, and CLI authority. Do not mint or project a
new generic GitHub credential from this TCFS ceremony. Any host with stale generic
GitHub material must converge through Lab's current-main attended switch path as a
separate incident gate.

This runbook does not restate the incident analysis — see TIN-2856's description and
its 2026-07-14 consumer-map comment, and TIN-2801's per-provider ledger. It defines
the *order of operations* and the *safety invariants*.

## The one invariant that can fork the fleet

The deployed unlock wrapper (lab `nix/home-manager/tummycrypt.nix`, unlock section)
derives the daemon master key as **SHA-256 of the passphrase-file bytes** and
**re-derives it on every unlock**. `tcfs rotate-key` as shipped mints its new key from
a mnemonic or `--password` via Argon2id + random salt. Rotating with a minted key
therefore diverges from what the wrapper re-derives at next login → **split-key
fleet** (explicitly forbidden by TIN-2856's acceptance).

Rule: **the new master key MUST equal SHA-256(new SOPS passphrase file bytes,
byte-for-byte, including any trailing newline).** Two ways to satisfy it:

1. `tcfs rotate-key --new-key-file <path>` — exact-key input
   (staged for this ceremony; see Phase 0).
2. Fallback with the shipped binary only: pre-seed the resume path — write the
   wrapper-derived key into `.master.key.rotate-pending` (0600) with a matching
   `.master.key.rotate-state.json`; `prepare_key_rotation` then consumes it.

Three passphrase→key derivations coexist and must stay convergent through the
ceremony: wrapper = SHA-256(file bytes); daemon/CLI `passphrase_file` fallback =
Argon2id + `crypto.kdf_salt` (`crates/tcfsd/src/daemon.rs`,
`crates/tcfs-crypto/src/recovery.rs`); FileProvider-direct = key/base64/mnemonic/
Argon2id + `encryption_salt` (`crates/tcfs-file-provider/src/direct.rs`). The fleet's
live path is the wrapper one; verify the other two are not silently in use on any
host before rotating (Phase 0 inventory).

Lab's current source pins signed release `v0.12.17`, which predates PR #558 and
does **not** provide `--new-key-file`; deployed host versions may lag that source
pin. A merge on `tummycrypt/main` is not an executable. Before any secret mutation,
Phase 0 must select and verify one reproducible executor. The preferred path is a
minimal signed `v0.12.18` maintenance release that backports PR #558 onto signed
`v0.12.17`; that path still requires an explicit release-policy exception and
exact-tag validation. The documented `v0.12.17` pending-file contract is
break-glass-only and requires an independent fixture rehearsal first. An exact-main
build is rejected because it includes unrelated post-release changes while still
reporting `0.12.17`; an ad hoc source build, moving branch, or unrecorded binary is
never ceremony authority.

## Credential inventory (paths and mechanisms only — never values)

| Material | Where | Mechanism |
|---|---|---|
| TCFS passphrase (leaked) | lab SOPS `nix/secrets/common.yaml` → `tcfs/encryption_passphrase`; materialized at `~/.config/sops-nix/secrets/tcfs/encryption_passphrase` (0400) | sops-nix; exported as `TCFS_ENCRYPTION_KEY_FILE` |
| Derived master key | `~/.local/state/tcfsd/master.key` (0600) | wrapper SHA-256(file bytes); `crypto.master_key_file` in `~/.config/tcfs/config.toml`; daemon auto-loads |
| FP plaintext embed | `~/.config/tcfs/fileprovider/config.json` (0600) | regenerated each HM activation with `encryption_passphrase` + `encryption_salt` |
| Keychain | macOS service `tcfs`: `master-key`, `device-identity`, `session-token` (`crates/tcfs-secrets/src/keychain.rs`) | check existence during inventory; do not print |
| iOS | Keychain + QR bootstrap payload (TIN-1424) | attended re-issue lane |
| Generic GitHub authority | lab SOPS `api.github_token` → `~/.config/sops-nix/secrets/api/github_token`; file-backed consumers remain separate from the personal packages-read credential | validate current source/runtime capability without printing values; stale hosts converge through an attended Lab switch, not this rotation |
| Personal packages read | lab SOPS `nix/secrets/operators/github-packages.yaml` → `api/github_packages_read_token` | PR #876 custody lane is complete; Honey/macbook-neo file-only projection; never substitute for generic GitHub auth |
| Operator TOTP vault | `~/.local/state/tcfs-operator/tcfs-totp.kdbx` + `.keyx` | not implicated; frozen-class ceremonies only |
| Stray path propagation | current Lab source excludes the TCFS key path from `dev.tinyland.prompt-pulse`; previously activated generations may still carry it; `/Library/LaunchAgents/io.tinyland.tcfsd` staleness remains TIN-1954 scope | verify prompt-pulse after activation; handle the root LaunchAgent through TIN-1954 |

## Ceremony

### Phase 0 — stage during freeze (all `[PREP-SAFE-NOW]`)

0.1 Confirm freeze posture: `LAB_DEPLOY_FREEZE` set; lab#825 gate live.
0.2 PR #558 is merged, but Lab source still pins signed `v0.12.17`, which predates
    `--new-key-file`, and deployed versions must be inventoried separately. Select
    exactly one executor path and record its absolute path, version, immutable source
    revision, artifact hash, and validation evidence:

- Preferred: a minimal signed `v0.12.18` maintenance release containing only the
  reviewed #558 backport plus release metadata. Obtain the release-policy exception
  first; bind `ROTATE_TCFS_BIN`, then require exact-tag Nix proof and
  `"${ROTATE_TCFS_BIN}" --version` output of `tcfs 0.12.18` before use.
- Break glass: the signed `v0.12.17` binary after a fixture rehearsal of its
  version-1 pending-state contract. The pending key must be exactly 32 raw bytes at
  `.master.key.rotate-pending`; the matching `.master.key.rotate-state.json` must
  name the current manifest prefix and that exact pending path. Hex input is invalid.
  Independently prove the pending bytes equal SHA-256 of the new passphrase-file
  bytes before the live command.

Reject an exact-main build, moving branch, or ad hoc local build. Bind the selected
absolute executable path as `ROTATE_TCFS_BIN`; do not rely on `PATH`.

0.3 ~~Fix lab `docs/operations/tcfs-fileprovider-deploy.md` `cat` guidance~~ —
    **already corrected on lab main** (verified 2026-07-16: the doc now forbids `cat`/
    xtrace/pretty-printers on the key material; landed via lab#844's containment slice).
0.4 Resolve the current Lab `tcfs_clients` inventory group, then classify every
    enrolled host as an active daemon/FileProvider/key consumer or package-only
    client. The current source group is honey, macbook-neo, petting-zoo-mini, yoga,
    bumble, mbp-13, and sting; Sting currently has its daemon, automount, MCP, and
    selective-sync surfaces forced off and must not be counted as an active writer.
    On every enrolled host, inspect file modes and presence for every applicable row
    above; inspect `launchctl` posture for TIN-1954 where applicable (redacted output
    only); and confirm no active consumer uses the passphrase-Argon2id or
    FP-mnemonic derivation path. Stop if source enrollment and live runtime differ.

The 2026-07-22 preflight is a starting snapshot, not ceremony authority:

| Host | Source role | Last observed runtime | Executable provenance |
|---|---|---|---|
| Honey | active TCFS client | daemon running | `v0.12.17` |
| macbook-neo | active TCFS client | daemon running | effective interactive PATH `v0.12.12` |
| petting-zoo-mini | active TCFS client | **daemon stopped: source/runtime drift** | record before ceremony |
| Yoga | active TCFS client | daemon running | record before ceremony |
| Bumble | active TCFS client | daemon running | record before ceremony |
| mbp-13 | active TCFS client | daemon running | record before ceremony |
| Sting | package-only; runtime surfaces forced off | stopped as designed | `v0.12.16` |

Replace every `record before ceremony` cell with an absolute executable path,
version, and immutable source provenance in the evidence packet. Re-run the status
probe immediately before quiescence; PZM's drift is a stop condition, not permission
to reclassify it as package-only.

0.5 Optional sizing: `tcfs key rotate <prefix>` (dry-run by default) to estimate
    forward-secrecy re-encryption cost for candidate prefixes.

### Phase 1 — GitHub credential reconciliation (superseded and separated)

1.1 Do not mint, import, or repurpose a GitHub credential here. PR #876's personal
    packages-read authority is a closed, separate lane and is not valid generic
    API/Nix/MCP/CLI authority.
1.2 Before the TCFS ceremony, run value-free capability checks against the current
    Lab `api/github_token` authority on each evidence-capture host. A stale host must
    receive a reviewed current-main Lab switch; do not edit `/etc/nix` or generated
    Home Manager files directly.
1.3 Record any remaining host-runtime drift on TIN-2801. It does not change the
    passphrase-to-master-key invariant and must not be folded into the key rotation.

### Phase 2 — TCFS passphrase leg (closes the TIN-2856 child gate)

2.1 `[SECRET-GATE]` Generate the new passphrase offline; `sops` edit
    `tcfs/encryption_passphrase`; **do not HM-switch yet**.
2.2 `[SECRET-GATE]` Derive the new master exactly as the wrapper does
    (SHA-256 of the *file bytes*); stage as `rotate-key` input.
2.3 Quiesce every active daemon/FileProvider writer identified by Phase 0 — single
    writer during manifest rewrite. Package-only clients are not writers, but remain
    in the post-rotation convergence ledger.
2.4 `[SECRET-GATE]` On Neo, invoke only the absolute executable recorded in Phase
    0.2. Set `OLD_MASTER_KEY_FILE` to the active absolute master-key path and
    `NEW_MASTER_KEY_FILE` to the separately staged 32-byte wrapper-derived key. Then
    use exactly the invocation for the selected path:

- Signed `v0.12.18` path:

  ```sh
  "${ROTATE_TCFS_BIN}" rotate-key \
    --old-key-file "${OLD_MASTER_KEY_FILE}" \
    --new-key-file "${NEW_MASTER_KEY_FILE}"
  ```

- Rehearsed signed `v0.12.17` break-glass path, only after the exact pending key and
  version-1 state file from 0.2 are present:

  ```sh
  "${ROTATE_TCFS_BIN}" rotate-key --old-key-file "${OLD_MASTER_KEY_FILE}"
  ```

Immediately before either command, re-run the recorded absolute-path version, hash,
pending-state, drain, and manifest-prefix checks. Expect **zero** per-device/keyless
skips (fleet `wrap_mode=master`); any skip is a stop-the-line finding. Resumability
is through the selected executor's pending/state contract.

2.5 `[SECRET-GATE]` Switch every host in the Phase 0 `tcfs_clients` ledger in the
    reviewed order, beginning with the rotation writer and active canaries. The new
    passphrase materializes, the wrapper re-derives `master.key`, and enabled
    FileProvider configuration regenerates. A package-only host still needs source
    convergence, but must not be promoted as daemon/FileProvider proof. Re-issue iOS
    through the attended lane (TIN-1424 caveat applies to the new QR).
2.6 Verify according to each host's declared role: daemon/FileProvider consumers get
    unlock plus encrypted round-trip; all enrolled hosts get source/runtime version,
    secret-path mode, and derivation-path checks. Run the **negative test: the old
    derived key must be rejected** on every active encryption consumer. Capture the
    evidence packet under the `docs/release/evidence/` convention.
2.7 `[SECRET-GATE]` Decision D2: forward secrecy — master rotate re-wraps key-wrapping
    only; content keys are unchanged, so pre-rotation manifest copies + the leaked
    passphrase still decrypt old content. Run
    `tcfs key rotate <prefix> --rotate-keys` for prefixes whose pre-rotation manifests
    may persist anywhere untrusted.
2.8 Retire: purge stale derived-key/FP-config copies; verify the landed Lab
    prompt-pulse allowlist excludes `TCFS_ENCRYPTION_KEY_FILE` after activation
    (do not duplicate that source fix); handle the stale root LaunchAgent through
    TIN-1954; sweep retained execution artifacts for any second passphrase copy and
    record the retention/deletion boundary (TIN-2856 acceptance).

### Phase 3 — close-out

3.1 Close TIN-2856 (rotation owner clears the child).
3.2 Resolve every remaining TIN-2801 provider-ledger row, including the Stripe
    dashboard roll and the other provider-specific replacement, cutover, revocation,
    and negative-proof gates. TIN-2856 closure does not waive another provider row.
3.3 Only when the ledger has zero `OPEN` rows: require provider-negative proof for
    every `CLOSED` row and the appropriate recorded rationale/evidence for every
    `ACCEPTED DISPOSITION` row. Then move TIN-2801 to Done, delete
    `LAB_DEPLOY_FREEZE`, and resume the gate ladder
    (TIN-2306 stop-rule is next; see the TIN-2306 runbook and its `[CRED]` convention,
    docs/ops/tin2306-stop-rule-runbook-2026-07-14.md, PR #554).

## Operator decisions — RATIFIED (2026-07-16 interview)

| # | Decision | Ruling |
|---|---|---|
| D1 | Fleet GitHub credential shape | **Superseded after #876.** This ceremony does not mint GitHub credentials. Personal packages-read custody is separate; generic GitHub host drift converges through Lab current-main switches. |
| D2 | Forward-secrecy scope | **None — rewrap only.** Residual accepted: old content remains decryptable to the leaked passphrase only if pre-rotation manifests also leaked; exposure boundary was execution telemetry and no manifest leak is in evidence. Step 2.7 is skipped; step 0.5 sizing optional. |
| D3 | iOS re-issue timing | Folded into D4's single window (attended). |
| D4 | Ceremony window | **One attended TCFS passphrase window** for Phase 2 after #557/#558, executor provenance, the live `tcfs_clients` census, and current Lab source are reviewed. Neo's YubiKey-dependent GPG-signing and SSH-to-GitHub recovery may be co-scheduled, but remains a separately governed Lab operation with its own preflight and proof; it is not a TIN-2856 result. Generic GitHub credential mutation remains separate and must not be added to this ceremony. |

## Related runbooks (linked, not duplicated)

- docs/ops/tin2306-stop-rule-runbook-2026-07-14.md (PR #554) — downstream gate; source
  of the `[CRED]` gating convention.
- lab `docs/operations/token-rotation.md` — Attic-specific and flagged inaccurate for
  the stateless-JWT reality (TIN-2814 / lab#822); pattern reference only.
- lab `docs/operations/tailscale-api-key-rotation.md` (TIN-2639 lane, separate),
  `step-ca-root-rotation-dr.md`, `SECRETS_SCHEMA.md`.
- docs/ops/per-device-crypto-migration-2026-06-06.md and
  docs/ops/onprem-authority-recovery.md — wrap-mode/authority context.
