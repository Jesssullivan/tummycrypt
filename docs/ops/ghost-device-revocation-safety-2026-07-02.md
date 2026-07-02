# Ghost-device revocation under master wrap — safety analysis (2026-07-02)

Status: analysis + parked operator packet. Doc-only; **no devices were revoked with this
document.** Written per the 2026-07-01 operator decision (ledger item 9: ghost-device
revocation is on the PerDevice critical path, committed by 2026-08-31) and the wave-1
instruction: if revocation semantics under current master-wrap are unclear, write the
safety analysis instead of revoking.

## The subjects

Live registry on neo (2026-07-02, `tcfs device list`, daemon fleet at v0.12.15):

| name | id | enrolled | read |
|---|---|---|---|
| `local-fileprovider-coordinated` | `d6d65d8d` | 2026-05-01 | FP test-lane identity (coordinated read path) |
| `local-fileprovider-data` | `f1d980ab` | 2026-05-01 | FP test-lane identity (data path) |
| `local-fileprovider-request-download` | `ff818f17` | 2026-05-01 | FP test-lane identity (requestDownload path) |

All three carry real `age1…` recipients and are `active`. They date from the May macOS
FileProvider bring-up lanes and have no corresponding physical device.

## What `tcfs device revoke <name>` actually does (verified in source, `main` @ 2f3c046)

- `cmd_device_revoke` (`crates/tcfs-cli/src/main.rs:4275`): marks `revoked=true` +
  `revoked_at` in the **local** registry, re-signs the envelope
  (`save_registry_signed_or_warn`, TIN-1417 B4), prints a loud forward-secrecy warning.
- It does **NOT** sync to the remote registry. Propagation is a separate step:
  `tcfs device enroll --sync-remote` merges the verified remote and republishes the signed
  merged registry to `<prefix>/tcfs-meta/devices.json` (`main.rs:4380–4414`).
- Merge is **revocation-sticky**: `merge_device_entry` only ever flips `revoked`
  false→true (`if … && incoming.revoked`), so a peer whose copy still lists the ghosts as
  active cannot resurrect them; unsigned-remote laundering is refused
  (`enforce_remote_merge_trust`), and tampered un-revocation fails signature verification
  (`tamper_unrevoke_fails_verification`).

## Effect matrix under each wrap mode

| wrap_mode | Effect of revoking the 3 ghosts |
|---|---|
| `master` (current fleet state) | **No cryptographic effect.** Content carries only the master wrap; every holder of the master key decrypts regardless of registry state. Revocation here is registry hygiene. |
| `dual` (EXPAND) | Ghosts are dropped from every recipient set built after the local revoke (`active_devices()` filter); new `wrapped_file_keys` exclude them. Master wrap still present, so no read denial yet. |
| `per_device` (CONTRACT) | True denial of NEW content. Historical content stays readable to any holder of previously-wrapped keys until `tcfs key rotate <prefix> --rotate-keys` (TIN-1899, landed). |

## Why this is on the critical path anyway (the roll-call coupling)

The dual→per_device promotion gate is a **roll-call** over every active (non-revoked)
device (`per-device-crypto-migration-2026-06-06.md`; `roll_call_revoked_devices_do_not_block`
test). Three ghost devices that can never ack would otherwise **permanently block the
contract flip**. Revoking them is what makes the roll-call gate satisfiable — that is the
substance of ledger item 9, independent of any crypto effect today.

## Residual risks before executing (why this is parked, not done)

1. **FP identity liveness (the one real unknown).** If neo's FileProvider extension is
   currently registered and using one of these identities, revoking it could fail-close FP
   operations once any registry-respecting path consults `revoked`. Pre-check on neo:
   `fileproviderctl domain list` (or Finder → Locations) — if no tcfs domain is active, the
   identities are provably ghosts. The FP test lanes that minted them are not in use on neo
   per `macos-fileprovider-reality.md` (testing-mode lane is non-production).
2. **Propagation ordering.** The revoke is only fleet-canonical after a signed
   `--sync-remote` republish from neo AND a merge pull on honey. Half-propagated state is
   harmless under `master` (no crypto effect) but should be closed before any `dual` flip.
3. **No re-key needed.** Under `master` there are no per-device wraps to rotate; skip
   `tcfs key rotate` for this operation.

## Parked operator packet (execute when rubber-stamped; ~2 min)

```bash
# 0. Pre-check (neo): prove no live FP domain uses the ghost identities
fileproviderctl domain list || true

# 1. Revoke locally on neo (signed registry, revoked_at stamped)
tcfs device revoke local-fileprovider-coordinated
tcfs device revoke local-fileprovider-data
tcfs device revoke local-fileprovider-request-download

# 2. Propagate: republish the signed merged registry to S3
tcfs device enroll --name neo.local --sync-remote

# 3. Converge honey (pulls + merges the signed remote; revocation is sticky)
ssh honey 'tcfs device enroll --name honey --sync-remote'

# 4. Verify both sides show REVOKED for all three ids d6d65d8d/f1d980ab/ff818f17
tcfs device list && ssh honey 'tcfs device list'
```

Evidence expectation: bank the four command outputs as a dated
`docs/release/evidence/ghost-device-revocation-<UTC>/` packet; then the negative roll-call
test (ledger item 9) can use the post-revocation registry as its fixture.
