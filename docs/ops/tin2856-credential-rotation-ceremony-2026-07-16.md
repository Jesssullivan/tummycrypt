# TIN-2856 Credential-Rotation Ceremony — Runbook (2026-07-16)

**Status:** DESIGNED-ONLY (ceremony not yet executed; this document is the plan).
**Owner:** operator (Jess). Agents may stage `[PREP-SAFE-NOW]` steps; every
`[SECRET-GATE]` step is operator-executed, attended, and interviewed before it runs.
**Incident:** TIN-2856 (TCFS encryption passphrase exposed to agent execution logs via
`bash -x` inheriting `TCFS_ENCRYPTION_KEY_FILE`, 2026-07-14), child of TIN-2801
(PR lab#721 shell-expanded env leak). Fold-ins: the dead fleet GitHub token
(TIN-2801 comment `42205716`), stale LaunchAgent exposure (TIN-1954), and the
QR-invite plaintext caveat (TIN-1424 — the new passphrase inherits the same QR
weakness until that ticket closes).
**Freeze:** `LAB_DEPLOY_FREEZE` + nix-deploy gate (lab#825) stay in force until
TIN-2801 closes. This ceremony is itself the unfreezing act for the TCFS legs; it
blocks TIN-2306, TIN-2658 close-out, TIN-1903/1904, TIN-1620, TIN-1546.

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

## Credential inventory (paths and mechanisms only — never values)

| Material | Where | Mechanism |
|---|---|---|
| TCFS passphrase (leaked) | lab SOPS `nix/secrets/common.yaml` → `tcfs/encryption_passphrase`; materialized at `~/.config/sops-nix/secrets/tcfs/encryption_passphrase` (0400) | sops-nix; exported as `TCFS_ENCRYPTION_KEY_FILE` |
| Derived master key | `~/.local/state/tcfsd/master.key` (0600) | wrapper SHA-256(file bytes); `crypto.master_key_file` in `~/.config/tcfs/config.toml`; daemon auto-loads |
| FP plaintext embed | `~/.config/tcfs/fileprovider/config.json` (0600) | regenerated each HM activation with `encryption_passphrase` + `encryption_salt` |
| Keychain | macOS service `tcfs`: `master-key`, `device-identity`, `session-token` (`crates/tcfs-secrets/src/keychain.rs`) | check existence during inventory; do not print |
| iOS | Keychain + QR bootstrap payload (TIN-1424) | attended re-issue lane |
| Fleet GitHub token (dead) | lab SOPS `api.github_token` → `~/.config/sops-nix/secrets/api/github_token`; `GH_TOKEN`/`GITHUB_TOKEN` via secret-vars; `/etc/nix/nix.custom.conf` `access-tokens` (root-owned); Actions `PERSONAL_PAT`, Dependabot, dispatch-token set | old token revoked 2026-07-13; interim = operator keyring OAuth; durable fix owed = least-privilege fleet credential |
| Operator TOTP vault | `~/.local/state/tcfs-operator/tcfs-totp.kdbx` + `.keyx` | not implicated; frozen-class ceremonies only |
| Stray path propagation | `dev.tinyland.prompt-pulse` LaunchAgent carries the key-file path (non-consumer); `/Library/LaunchAgents/io.tinyland.tcfsd` staleness (TIN-1954) | retire in Phase 2 step 16 |

## Ceremony

### Phase 0 — stage during freeze (all `[PREP-SAFE-NOW]`)

0.1 Confirm freeze posture: `LAB_DEPLOY_FREEZE` set; lab#825 gate live.
0.2 Land `tcfs rotate-key --new-key-file` (tummycrypt PR; merge ≠ deploy; **no tag or
    release under the freeze**). Fallback if not landed: pending-file pre-seed above.
0.3 Fix lab `docs/operations/tcfs-fileprovider-deploy.md` — replace the "`cat` the key
    file" instruction with metadata-only checks (lab PR).
0.4 Read-only inventory on each host (neo/honey/bumble): file modes and presence for
    every row above; `launchctl` posture for TIN-1954 (redacted output only); confirm
    no host is on the passphrase-Argon2id or FP-mnemonic derivation path.
0.5 Optional sizing: `tcfs key rotate <prefix>` (dry-run by default) to estimate
    forward-secrecy re-encryption cost for candidate prefixes.

### Phase 1 — GitHub token leg (first: restores evidence capture)

1.1 `[SECRET-GATE]` Mint the least-privilege fleet credential (fine-grained PAT vs
    GitHub App — operator decision D1 below).
1.2 `[SECRET-GATE]` `sops` edit `api.github_token`; HM switch **neo → honey → pzm**
    (folds the owed runtime convergence); root-edit `/etc/nix/nix.custom.conf`
    `access-tokens`; update `PERSONAL_PAT`/Dependabot/dispatch secrets; restart MCP
    consumers. (Note lab branch `security/credential-rotation-20260715` already stages
    a `common.yaml` refresh — rebase or supersede, don't double-project.)
1.3 Verify: `gh api /user` 200 with the new credential on each host; private flake
    fetch OK; old fingerprint absent from runtime env everywhere.

### Phase 2 — TCFS passphrase leg (the unfreezing act)

2.1 `[SECRET-GATE]` Generate the new passphrase offline; `sops` edit
    `tcfs/encryption_passphrase`; **do not HM-switch yet**.
2.2 `[SECRET-GATE]` Derive the new master exactly as the wrapper does
    (SHA-256 of the *file bytes*); stage as `rotate-key` input.
2.3 Quiesce honey/bumble sync — single writer during manifest rewrite.
2.4 `[SECRET-GATE]` On neo:
    `tcfs rotate-key --old-key-file ~/.local/state/tcfsd/master.key --new-key-file …`
    Expect **zero** per-device/keyless skips (fleet `wrap_mode=master`); any skip is a
    stop-the-line finding. Resumable via the pending/state files if interrupted.
2.5 `[SECRET-GATE]` Per host neo → honey → bumble: HM switch (new passphrase
    materializes → wrapper re-derives `master.key` → FP `config.json` regenerates);
    iOS re-issue via the attended lane (TIN-1424 caveat applies to the new QR).
2.6 Verify per host: daemon unlock + encrypted round-trip; **negative test — the old
    derived key must be rejected**; capture the evidence packet
    (docs/release/evidence/ convention).
2.7 `[SECRET-GATE]` Decision D2: forward secrecy — master rotate re-wraps key-wrapping
    only; content keys are unchanged, so pre-rotation manifest copies + the leaked
    passphrase still decrypt old content. Run
    `tcfs key rotate <prefix> --rotate-keys` for prefixes whose pre-rotation manifests
    may persist anywhere untrusted.
2.8 Retire: purge stale derived-key/FP-config copies; drop the `prompt-pulse` key-file
    path and resolve TIN-1954's LaunchAgent posture; sweep retained execution
    artifacts for any second passphrase copy; record the retention/deletion boundary
    (TIN-2856 acceptance).

### Phase 3 — close-out

3.1 Close TIN-2856 (rotation owner clears the child).
3.2 Remaining TIN-2801 gates: Stripe live-key dashboard roll `[SECRET-GATE,
    operator-only]` + the residual provider list in the TIN-2801 ledger.
3.3 TIN-2801 → Done; delete `LAB_DEPLOY_FREEZE`; resume the gate ladder
    (TIN-2306 stop-rule is next; see the TIN-2306 runbook and its `[CRED]` convention,
    docs/ops/tin2306-stop-rule-runbook-2026-07-14.md, PR #554).

## Operator decisions (interviewed before the secret gates run)

| # | Decision | Options |
|---|---|---|
| D1 | Fleet GitHub credential shape | fine-grained PAT (fast) vs GitHub App (durable custody; TIN-2801 notes App custody owed for dispatch tokens) |
| D2 | Forward-secrecy scope | none (accept old-content exposure to the leaked passphrase) / sensitive prefixes / all prefixes (cost from 0.5 dry-run) |
| D3 | iOS re-issue timing | inside Phase 2 window vs deferred attended session (device offline risk) |
| D4 | Ceremony window | single attended window for Phases 1+2 vs GitHub leg early + passphrase leg in its own window |

## Related runbooks (linked, not duplicated)

- docs/ops/tin2306-stop-rule-runbook-2026-07-14.md (PR #554) — downstream gate; source
  of the `[CRED]` gating convention.
- lab `docs/operations/token-rotation.md` — Attic-specific and flagged inaccurate for
  the stateless-JWT reality (TIN-2814 / lab#822); pattern reference only.
- lab `docs/operations/tailscale-api-key-rotation.md` (TIN-2639 lane, separate),
  `step-ca-root-rotation-dr.md`, `SECRETS_SCHEMA.md`.
- docs/ops/per-device-crypto-migration-2026-06-06.md and
  docs/ops/onprem-authority-recovery.md — wrap-mode/authority context.
