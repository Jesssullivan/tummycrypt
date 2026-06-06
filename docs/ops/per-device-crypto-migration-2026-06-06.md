# Shared-Master to Per-Device Crypto Migration Plan (TIN-1417)

Status: operational migration plan. Doc-only. No code lands with this document;
no fleet config flips with this document. This is the verified, ratified
sequencing the fleet follows to move from the shared-master-key model to
per-device age/X25519 FileKey wrapping. It is an acceptance artifact for the
TIN-1417 design (`docs/ops/per-device-crypto-identity-design-2026-05-18.md`) and
should be read as the operator-facing companion to that design recon.

Date: 2026-06-06.

Grounding: every primitive, phase, and revocation claim here derives from
`docs/ops/per-device-crypto-identity-design-2026-05-18.md` (the "design doc"
below). Where this plan adds operational structure not in the design doc — an
expand/contract ordering, a `per_device_wrap_strict` gate, per-step rollback —
it is called out as such and grounded in the verified code state on
`origin/main` as of this date.

## Why expand/contract

The design doc's Migration Path (design doc lines 198–224) lays out Phase 0
through Phase 4 as a schema evolution. This plan re-expresses that as a strict
**expand-then-contract** discipline so that at no point is there a fleet state
where a healthy, non-revoked device cannot read content it is entitled to read:

- **Expand**: every writer emits BOTH the legacy master wrap
  (`encrypted_file_key`) and the per-device wraps (`wrapped_file_keys`) — dual
  write. Any reader, upgraded or not, can decrypt. This is design doc Phase 1
  (line 207).
- **Contract**: only after a green fleet roll-call do writers stop emitting the
  legacy master wrap, per manifest, one direction. This is design doc Phase 2/3
  (lines 212–219), gated behind a new `per_device_wrap_strict` flag this plan
  introduces.

The existing `crypto.per_device_wrapping` flag (default false, verified at
`crates/tcfs-core/src/config.rs:337,351`) is the EXPAND switch: it makes writers
ADD per-device wraps. It does not, by itself, remove the master wrap — and this
plan requires that it never does. The CONTRACT switch is a separate, new
`per_device_wrap_strict` flag (does not exist in tree yet — verified absent on
`origin/main`) that gates dropping the master wrap. Splitting expand from
contract is the core safety property of this migration.

Default-off behavior MUST stay byte-identical: with `per_device_wrapping=false`
the engine takes `EncryptionContext::new(master_key)` and the legacy single-wrap
path unchanged (verified: daemon `build_encryption_context` returns `base` early
when the flag is false, `crates/tcfsd/src/grpc.rs:39-42`).

## Migration Sequence

### Step 1 — B1 FileProvider parity (expand, prerequisite)

Bring the FileProvider direct restore path to crypto parity with the daemon and
CLI before any per-device wrapping is enabled anywhere on the fleet.

Verified gap: `crates/tcfs-file-provider/src/grpc_backend.rs:232-234` builds
`EncryptionContext::new(mk)` only — device identity is `None`, it never calls
`.with_device_wrapping`. The engine read switch
(`crates/tcfs-sync/src/engine.rs:2226-2236`) takes the `wrapped_file_keys`-
non-empty branch FIRST and hard-fails ("manifest is per-device encrypted but
this device has no age identity") with no master fallback. So the moment any
per-device manifest reaches a FileProvider hydrate, hydration breaks. The daemon
(`crates/tcfsd/src/grpc.rs:32-83`, gated on `crypto.per_device_wrapping` at :40,
`.with_device_wrapping` at :82) and CLI (`crates/tcfs-cli/src/main.rs:1108-1158`)
already wire device identity. The FileProvider crate has no
`build_encryption_context` helper — Step 1 adds one, mirroring the daemon's
fail-closed-to-master-readable logic.

Step 1 lands BEFORE the flag is enabled on any host, so it is a no-op while every
manifest is still legacy single-wrap. It only matters once Step 3 puts
per-device manifests on the wire.

Note: the non-prod default/uniffi backends are worse than grpc_backend — they
match only on `encrypted_file_key`, fall to `_ => None`, and copy chunks as raw
ciphertext (silent corruption). Step 1 must either fix or explicitly fence these
backends so a per-device manifest cannot reach them and corrupt silently.

### Step 2 — B3 restore Phase-1 dual-write + roll-call gate (expand)

Wire per-device dual-write through every restore caller and introduce the
roll-call gate behind the new `per_device_wrap_strict` flag (default false).

Three verified production restore callers must all honor per-device unwrap with
master fallback: `crates/tcfs-sync/src/reconcile.rs:906` and `:1068` (auto-roam
Pull), `crates/tcfs-file-provider/src/grpc_backend.rs:236` (FileProvider, fixed
in Step 1), and `crates/tcfs-cli/src/main.rs:1436` (CLI pull). Upload side:
`upload_symlink_with_device` (`crates/tcfs-sync/src/engine.rs:1995`) and the
single wrap call site referenced in the design doc (line 240).

In this step writers emit BOTH wraps (design doc Phase 1, line 207). The new
`per_device_wrap_strict` flag is added but stays false; it only later (Step 7)
authorizes dropping the master wrap. The roll-call probe (design doc line 216 —
"gate the flag on a fleet roll-call probe that confirms every active device is
on the new binary") is built here and must read green before Step 7.

### Step 3 — Canary both directions including FileProvider hydrate (expand)

Enable `crypto.per_device_wrapping=true` on a small canary subset (still
dual-write, master wrap retained). Exercise push from canary, pull on canary,
pull on a legacy peer, and — critically — FileProvider hydrate on macOS, which is
the path Step 1 fixed and the one most likely to regress. Verify every direction
decrypts. Because the master wrap is still present, a legacy or
identity-missing reader still succeeds via fallback.

### Step 4 — B4 sign devices.json (expand)

Sign the device registry (`meta_prefix/tcfs-meta/devices.json`,
`crates/tcfs-secrets/src/device.rs` load_remote at line 145) so that recipient
sets and revocations cannot be forged by a peer. The design doc makes the S3
registry the canonical revocation authority (design doc decision 3, lines 96–98)
and requires that an unrelated device cannot mint a revocation record (design doc
Tests, lines 264). Recipient-set integrity must be established before strict mode
makes recipient-set membership the sole gate on readability.

### Step 5 — B2 per-device key rotation `tcfs key rotate <prefix>` (expand-capable)

Land the scoped rotation command (design doc lines 184–195, 250). This is the
ONLY mechanism that delivers forward secrecy after a revocation: it generates
fresh FileKeys, re-chunks and re-uploads under new BLAKE3 addresses, and
republishes manifests wrapped only to the post-revocation recipient set. It is
expensive (full re-push of the rotated set) and must surface projected
bytes-to-rewrite before the operator confirms (design doc line 194). Landing it
before the flag flip means revocation has a working remedy the day strict mode
goes live.

### Step 6 — Keychain hardening (expand)

Move device secret halves into the OS keychain (macOS Keychain, Linux
secret-service / file-fallback, Windows DPAPI) per design doc lines 78–81 and
decision 2 (lines 92–95). The Phase 0 `0600` file fallback
(`device-<device_id>.age`) is acceptable for dev/test with an explicit
insecure-mode marker but is not the production posture. Harden before strict mode
so a lost-laptop revocation is meaningful at the secret-storage layer, not just
the registry layer.

### Step 7 — Flag flip LAST: strict after green roll-call (contract)

Only now, after Steps 1–6 and a green roll-call, flip
`per_device_wrap_strict=true` so writers stop emitting the legacy master wrap
(design doc Phase 2/3, lines 212–219). The flip is gated on the roll-call probe
confirming every active device is on a binary that can unwrap per-device
manifests. The flip is one-way PER MANIFEST: a manifest written strict drops the
master wrap on its next version only; already-dual-written manifests stay
master-readable. Pull paths keep accepting both wraps indefinitely (design doc
decision 7, lines 108–110, and Phase 3 line 217 — "keep the deserialiser").

Master-key retirement (design doc Phase 4, lines 220–224) is explicitly OUT OF
SCOPE for this migration and tracked as TIN-1417-followup.

## Rollback (per step)

The migration is reversible at every step because expand never removes a wrap:

- **Steps 1–2 (code lands, flag off)**: revert the PR. No manifest on the wire
  changed; default-off behavior was byte-identical throughout.
- **Step 3 (canary, `per_device_wrapping=true`)**: set
  `crypto.per_device_wrapping=false`. Writers revert to legacy single-wrap.
  Every manifest written during the canary is dual-written and therefore stays
  master-readable by every device — nothing is stranded.
- **Step 4 (signed registry)**: revert to unsigned; signing is additive
  verification, not a wrap change.
- **Step 5 (rotation)**: rotation is operator-initiated and idempotent at the
  prefix level; do not run it, or roll forward only the prefixes you intend.
- **Step 6 (keychain)**: file fallback remains the documented escape hatch.
- **Step 7 (strict)**: flip `per_device_wrap_strict=false`. Writers resume
  dual-write immediately. The one-way property is PER MANIFEST and only takes
  effect after a green roll-call, so the blast radius of a bad flip is bounded to
  manifests written while strict was on; those manifests are still readable by
  every then-active recipient (that is the roll-call precondition). Dual-written
  manifests from before the flip remain master-readable regardless.

Invariant: dual-written manifests stay master-readable; the strict (master-wrap-
drop) transition is one-way per manifest and only after roll-call.

## Honest Claim Boundary

Revocation denies NEW content only. This is the load-bearing honesty statement
from the design doc (Revocation Semantics, lines 164–195) and it does not change
under this plan:

- A revoked device CANNOT decrypt manifests written after the revocation
  propagates AND after those files are next rewritten/rekeyed.
- A revoked device CAN still decrypt anything it already pulled (it holds the
  FileKey forever — "there is no cryptographic time machine", design doc line
  180) and anything still carrying a wrap it can open.
- Critically: content that is already-pulled, OR not-yet-rekeyed, stays
  master-decryptable until `tcfs key rotate <prefix>` re-chunks and rewraps it.
  Revocation alone does NOT rewrite historical manifests (design doc lines
  151–162). Forward secrecy is a SEPARATE, explicit, expensive operator action.

State this plainly in operator and CHANGELOG messaging: "Revoking a device stops
it from reading newly written content. It does not retroactively lock the device
out of content it already synced, and it does not lock it out of unchanged files
until you run `tcfs key rotate`."

## Ratified Seven Questions (design doc lines 280–312)

1. **age vs hpke vs Noise**: Lock to age/X25519 for M11; it is already in tree
   (design doc decision 1). Revisit HPKE only if stanza shape becomes awkward.
2. **Keychain coverage**: Fold the keychain trait into `tcfs-secrets` initially
   (design doc decision 2); no separate crate yet, file fallback dev/test only.
3. **Revocation propagation transport**: S3 registry is canonical authority;
   NATS is advisory-only and DEFERRED for this migration (design doc decision 3).
4. **Forward secrecy default**: Manual — `tcfs device revoke` prints the gap and
   offers `--rotate-keys`; no surprise multi-GB rewrites (design doc decision 4).
5. **Multi-tenant boundary**: Recipient sets scoped per-prefix for the first cut
   (design doc decision 5); per-file deferred to TIN-1418.
6. **Old PII in invites**: Treat plaintext-secret invites as a user-visible
   security correction with a CHANGELOG/security note (design doc decision 6).
7. **Backward-compat window**: Keep v2 single-wrap reads indefinitely; stop
   WRITING v2 only after the strict cutover gate (design doc decision 7).

## Residual Risks

- **Manual / partial forward secrecy**: revocation is not forward-secret by
  itself; the operator must run `tcfs key rotate <prefix>` per affected prefix,
  and any prefix not rotated stays master-decryptable to the revoked device.
- **Deferred NATS advisory leg**: sub-second revocation propagation is out of
  scope; the fleet accepts S3 eventual-consistency timing and must document the
  propagation window.
- **AES-SIV filename determinism**: deterministic filename encryption leaks
  equality/structure of names across the recipient set; per-device wrapping does
  not change this and it is not addressed here.
- **Headless-Linux keychain**: the secret-service path is not clean on headless
  Linux daemons (design doc decision 2 / question 2); file fallback remains the
  documented escape hatch and a known weaker posture there.
- **Invite authenticity**: invite signing and removal of plaintext storage
  secrets is TIN-1416 / TIN-1424, not this migration. Production self-enrollment
  stays disabled until invites no longer carry raw long-lived credentials
  (design doc lines 116–119, 240–246).
- **Symlink restore (PR-A)**: the restore path materializes a peer-published
  symlink target verbatim — `create_local_symlink`
  (`crates/tcfs-sync/src/engine.rs:2415-2441`) calls
  `std::os::unix::fs::symlink(target)` at `:2427` with no deny-set, traversal, or
  absolute-path guard, reached from `download_file_with_device`
  (`engine.rs:2114`) via all three production restore callers. A hostile peer can
  publish a `SymlinkManifest` targeting `../../.ssh/authorized_keys` and every
  pulling host materializes it. Per-device wrapping does NOT mitigate this — a
  legitimate-but-compromised or hostile recipient is still a valid recipient.
  Tracked separately as PR-A and must be fixed independently of this migration.

---

This plan is the operator-facing sequencing for TIN-1417. Land alignment in PR
comments. No flag flips and no fleet mutation occur from merging this doc.
