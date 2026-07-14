# Ghost-device revocation — EXECUTED — 2026-07-10T01:07Z

**Closes TIN-2347 (ledger item 9 execution): the three May FileProvider test-lane
ghost identities are REVOKED fleet-wide.** This is the dated evidence packet
required by TIN-2347's acceptance, executing the parked operator packet in
[`docs/ops/ghost-device-revocation-safety-2026-07-02.md`](../../../ops/ghost-device-revocation-safety-2026-07-02.md).
All commands were run inline by the orchestrator and operator-ratified.

## Scope

Revoke and fleet-propagate the three ghost devices (no physical device, minted by
the May 2026 macOS FileProvider bring-up lanes):

| name | id |
|---|---|
| `local-fileprovider-coordinated` | `d6d65d8d` |
| `local-fileprovider-data` | `f1d980ab` |
| `local-fileprovider-request-download` | `ff818f17` |

Fleet is under `wrap_mode=master`, so this is registry hygiene with no crypto
effect today; its substance is unblocking the dual→per_device roll-call gate
(the ghosts could never ack and would permanently block the contract flip).

## Execution vs the parked packet (4 steps + one detour)

| Step (safety doc) | Planned | As executed | Outcome |
|---|---|---|---|
| 0. Pre-check (neo) | `fileproviderctl domain list` proves no live FP domain uses the identities | Run 2026-07-09 | Clean: only a dead FileProvider domain expired 112 days prior. Identities provably ghosts. |
| 1. Revoke locally (neo) | `tcfs device revoke` ×3 | Run 2026-07-09 | All three marked REVOKED in the signed local registry on neo. |
| 2. Propagate to remote | plain `tcfs device enroll --sync-remote` | **Detour — B4 guard fired** (see below); resolved 2026-07-10T01:07:56Z via ratified one-time `--accept-unsigned-remote` on neo (master-key device `03d8a0bd`, signing_key `b0116055ea88c2f6`) | Remote merged + re-signed under the master key at `data/tcfs-meta/devices.json`. Verify: plain `--sync-remote` then succeeds with NO unsigned warning; device set unchanged (6 devices). |
| 3. Converge honey | `enroll --sync-remote` on honey | Run ~2026-07-10T03:30Z on honey (`d1176e5d`) | Pulled the signed registry cleanly, no flag needed. |
| 4. Verify both sides | `tcfs device list` on neo + honey | Run on both | All 3 ghosts show REVOKED on neo and honey. |

### The B4 detour (TIN-1417)

The safety doc's step 2 assumed plain `--sync-remote` would republish. In
practice the remote registry in seaweedfs-tcfs S3 was still **UNSIGNED legacy**,
and the TIN-1417 B4 guard refused, verbatim:

> TIN-1417 B4: refusing to merge an UNSIGNED (legacy) remote device registry on
> the enroll --sync-remote path. Merging then re-signing it with this device's
> master key would launder any attacker-injected recipient into a validly-signed
> registry.

This is the guard working as designed. Resolution: an operator-ratified,
one-time `tcfs device enroll --sync-remote --accept-unsigned-remote` on neo
(2026-07-10T01:07:56Z), which merged and re-signed the remote under the master
key. As a side effect the fleet remote registry is now signed — honey's pull
needed no flag. Evidence banked on TIN-1417 as comment `ef75427e`.

## Per-host verification

| Host | Device id | State |
|---|---|---|
| neo | `03d8a0bd` (master-key device) | All 3 ghosts signed-REVOKED locally (2026-07-09); re-signed remote published 2026-07-10T01:07:56Z; plain `--sync-remote` clean, 6 devices. |
| remote (seaweedfs-tcfs S3) | — | `data/tcfs-meta/devices.json` now signed under the master key; ghosts REVOKED. |
| honey | `d1176e5d` | `enroll --sync-remote` ~2026-07-10T03:30Z pulled signed registry cleanly; all 3 ghosts REVOKED. |
| Mac.localdomain | `be005d70` | Pending: picks up on next sync. Safe — merge is revocation-sticky (`merge_device_entry` only flips `revoked` false→true), so its stale active copies cannot resurrect the ghosts. |

## Residual notes

- **Mac.localdomain convergence** is the only open edge; harmless under
  `master` and closed automatically by its next `--sync-remote`.
- **Negative roll-call fixture now available (TIN-1904 / ledger item 9):** the
  post-revocation registry is the fixture the negative roll-call test expected
  (`roll_call_revoked_devices_do_not_block`).
- **No re-key performed**, per the safety analysis: under `master` there are no
  per-device wraps to rotate.

## Links

- TIN-2347 — closed Done with comment `9a4f9b8c` (this packet is its acceptance evidence)
- TIN-1417 — B4 guard evidence comment `ef75427e`
- Decision-ledger entry `R-b4` — merged via prompts-enqueue PR #106
- Safety analysis: `docs/ops/ghost-device-revocation-safety-2026-07-02.md`
