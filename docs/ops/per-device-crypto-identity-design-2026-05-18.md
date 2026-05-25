# Per-Device Cryptographic Identity Design (TIN-1417)

Status: design recon for M11 sprint planning. Not implementation. This document
proposes the architecture; the user reviews and aligns before any code lands.

Date: 2026-05-18.

2026-05-25 implementation note: the Phase 0 local-key slice has now started.
`tcfs init` and `tcfs device enroll` generate real age/X25519 device keys and
persist the secret half in a `0600` `device-<device_id>.age` file beside the
registry. Manifest wrapping, revoke semantics, pairing, and remote registry
trust are still open.

## Problem Statement

Today every "enrolled" device on a tcfs fleet holds the same shared master key
and there is no per-device cryptographic identity in the local enrollment path.
Concretely:

- `crates/tcfs-cli/src/main.rs::cmd_device_enroll` (around line 2950) stamps a
  placeholder public key:
  `format!("age1-device-{}", &blake3::hash(name.as_bytes()).to_hex()[..8])`.
  No keypair is generated; the secret half does not exist.
- `crates/tcfs-secrets/src/device.rs` has an `enroll_remote` helper (line 179)
  that DOES generate a real `age::x25519::Identity` and return the secret to the
  caller. The local CLI flow does not call it. So we have one usable code path
  for real keys but the most-used enrollment command sidesteps it.
- `DeviceRegistry::revoke` (`crates/tcfs-secrets/src/device.rs:84`) sets
  `revoked: true` in a JSON blob and saves to disk. The revoked device already
  holds the shared master key, which derives all FileKeys at decrypt time via
  `tcfs_crypto::unwrap_key` (`crates/tcfs-sync/src/engine.rs:2055`). Cutting it
  off the registry does not cut it off the data.
- The manifest schema (`crates/tcfs-sync/src/manifest.rs:35`) carries a single
  `encrypted_file_key: Option<String>` — one wrap, under the shared master.
  There is no recipient set, so there is nothing to remove a device from.
- `tcfs-auth/src/enrollment.rs` ships `EnrollmentInvite` payloads carrying
  plaintext S3 access keys, secret keys, and the encryption passphrase
  (`storage_access_key`, `storage_secret_key`, `encryption_passphrase` —
  lines 49–55). The invite is authenticated with `blake3::keyed_hash` under the
  current master key. Anyone holding the master key can mint invites, and any
  invite captured in transit leaks the full fleet master credential set.

Net effect: "revoked" devices retain forever-read on every chunk they have
ever pulled and every chunk reachable with the credentials they already saw.
This blocks meaningful revocation, blocks the per-tenant isolation work in
TIN-1418, blocks the pairing-based self-enrollment flow in TIN-1424, and blocks
per-device subscription scope in TIN-1416 because there is no per-device
identity to scope to.

## Proposed Primitives

Use age X25519 throughout. The infra already ships `age` for SOPS
(`crates/tcfs-sops/`) and `tcfs-secrets/src/device.rs:185` already builds a real
`age::x25519::Identity`. Standardising on age keeps one cryptographic library,
one key format, and one recipient encoding (`age1...`) across SOPS, device
identity, and per-recipient FileKey wrap.

New shape for `DeviceIdentity` in `tcfs-secrets`:

- `public_key: String` — real `age1...` recipient (today: placeholder).
- `signing_pubkey: String` — Ed25519 verifying key. age X25519 is for
  encryption only; we need a separate signature primitive for invite minting,
  device-authored manifests, and peer-to-peer auth.
- `enrolled_by: Option<String>` — device_id of the inviter, so the registry
  forms a graph instead of a flat list. Needed for revocation propagation
  decisions.
- `enrolled_at`, `name`, `device_id`, `revoked`, `revoked_at: Option<u64>` —
  retained.

The secret halves live in the OS keychain (macOS Keychain, Linux
secret-service / file-fallback, Windows DPAPI). Never in the JSON registry.
TIN-1417 implementation work owns the keychain abstraction; today
`tcfs-secrets` only handles SOPS/age decryption and the JSON registry.

## 2026-05-19 Decision Log

The M11 implementation should use these calls unless a later design review
finds a concrete blocker:

1. **Use age/X25519 for M11.** Do not pause Phase 0 for an HPKE/Noise bake-off.
   The project already ships `age`, and `tcfs-secrets` already has a real age
   enrollment helper. Revisit HPKE only if age's file/container shape becomes
   awkward for per-recipient FileKey stanzas.
2. **Keep the keychain trait in `tcfs-secrets` initially.** A separate
   `tcfs-keychain` crate is premature; start with a small trait plus platform
   implementations. File fallback is acceptable only for tests/dev with an
   explicit insecure-mode marker.
3. **Make S3 registry the canonical revocation authority; use NATS only as an
   advisory invalidation path.** NATS may speed up local cache refresh, but a
   broadcast event must never be the durable source of revocation truth.
4. **Do not auto-rotate on revoke by default.** `tcfs device revoke` must print
   the forward-secrecy gap and offer `--rotate-keys <prefix>` / a follow-up
   command, but surprise multi-GB rewrites are not acceptable as the default.
5. **Scope recipient sets per prefix for the first cut.** Per-file recipient
   sets are more flexible but make selective sync, audit, and UI explanation
   harder. Per-prefix maps cleanly to the expected subscription model.
6. **Treat plaintext-secret invites as a user-visible security correction.**
   The rollout needs a CHANGELOG/security note and migration guidance; do not
   silently paper over the old invite shape.
7. **Keep v2 single-wrap manifest reads indefinitely, but stop writing v2 after
   the cutover gate.** Historical data should remain readable without forcing a
   fleet-wide rewrite on day one.

Two hard gates fall out of those calls:

- Invite signatures must cover the complete canonical invite payload, including
  storage scope reference, nonce, expiry, creator, intended recipient pubkey,
  tenant/prefix, and bootstrap policy. Signing only metadata is not sufficient.
- Production self-enrollment stays disabled until invites no longer carry raw
  long-lived storage credentials and the new device brings its own real public
  key for approval/wrap.

New APIs in `tcfs-crypto`:

- `wrap_for_recipients(file_key, recipients: &[AgeRecipient]) -> Vec<WrappedKey>`
  — produces one `WrappedKey { recipient_id, ciphertext }` per recipient,
  matching age's stanza model.
- `unwrap_with_identity(wrapped: &[WrappedKey], my_identity: &AgeIdentity)
  -> Result<FileKey>` — finds the stanza addressed to us, returns the FileKey.
- Keep the existing master-key `wrap_key`/`unwrap_key` paths for the migration
  window; mark them `#[deprecated]` once parity is reached.

## Per-Device FileKey Wrapping Model

Each pushed manifest carries a vector of wrapped FileKeys instead of one. The
push path replaces the current single `encrypted_file_key` with a list:

```
wrapped_file_keys: Vec<WrappedFileKey {
  recipient_device_id: String,
  ciphertext: String,    // base64(age stanza)
}>
```

Recipient set rules:

- **Who decides recipients**: at push time the engine reads the active device
  registry from local cache (mirrored from `meta_prefix/tcfs-meta/devices.json`,
  see `DeviceRegistry::load_remote` at `tcfs-secrets/src/device.rs:145`) and
  wraps the FileKey for every non-revoked device whose public_key parses as a
  valid age recipient. The pushing device must include itself.
- **What triggers rewrap**: a new manifest version is published whenever any of
  these change: file contents, the recipient set membership, or a recipient's
  public key. A revocation alone does NOT rewrap historical manifests — see
  next section.
- **Old chunks after revocation**: chunks are content-addressed by BLAKE3
  (`crates/tcfs-chunks/`). They are encrypted with the FileKey, not the master
  key. A revoked device that already pulled those chunks already has the
  FileKey; nothing we do server-side affects bytes already on disk. New
  manifests stop including a wrapped key for the revoked recipient; the chunk
  blobs themselves are unchanged. If the operator wants forward secrecy too,
  they must rotate FileKeys (re-chunk + re-upload affected files), which is
  effectively a full re-push for the rewrap set. This cost is explicit and the
  CLI must surface it.

## Revocation Semantics

Be honest about what revocation can and cannot do.

**Can do**:

- Stop the revoked device from decrypting NEW manifests pushed after the
  revocation propagates.
- Stop the revoked device from being included in future invite chains.
- Stop the revoked device from authenticating to NATS/S3 if its credentials
  are per-device (today they are not — that is TIN-1416 territory).

**Cannot do, ever, without re-encryption**:

- Un-decrypt content the device already pulled. Once a device received a
  FileKey wrapped to its recipient, it has the FileKey forever. There is no
  cryptographic time machine.
- Stop the device from reading existing chunks it has cached. The chunk
  ciphertext is symmetric; the device already has the symmetric key.

If forward secrecy is required at revocation time, the operator runs
`tcfs key rotate <path-prefix>` (new command, scoped) which:

1. Generates fresh FileKeys for each affected file.
2. Re-chunks and re-uploads under new BLAKE3 addresses.
3. Publishes new manifests wrapping the new FileKeys to the current
   (post-revocation) recipient set.
4. Optionally garbage-collects old chunks once all live manifests stop
   referencing them.

This is expensive (full re-push of the rotated set) and the CLI surfaces
projected bytes-to-rewrite before the operator confirms.

## Migration Path

Existing fleets are on shared-master-key model with `encrypted_file_key:
Option<String>` (one wrap). Phased rollout:

1. **Phase 0 (started 2026-05-25, additive only)**: real keypair generation
   inside local `tcfs init` and `tcfs device enroll`. Persist the secret half in
   a protected file fallback for now; the keychain-backed store remains a
   follow-up. No manifest format change yet. Existing fleets keep working
   unchanged because the new per-device key is unused at the wrap layer.
2. **Phase 1 (manifest schema v3)**: add `wrapped_file_keys:
   Vec<WrappedFileKey>` alongside the legacy `encrypted_file_key`. Push paths
   write both. Pull paths prefer `wrapped_file_keys` and fall back to the
   legacy single-wrap if missing or if no stanza addresses this device. Mixed
   fleets work in both directions.
3. **Phase 2 (engine cutover)**: a fleet config flag
   `crypto.per_device_wrap = true` makes push paths stop writing the legacy
   single-wrap. Pull paths still accept it for backfill. Devices that have not
   upgraded see new manifests and fail to decrypt — gate the flag on a fleet
   roll-call probe that confirms every active device is on the new binary.
4. **Phase 3 (legacy removal)**: drop the `encrypted_file_key` field from new
   writes; keep the deserialiser for at least one minor cycle for historical
   manifests.
5. **Phase 4 (master-key retirement)**: the shared master key only protects
   manifest-key/name-key HKDF derivations (`tcfs-crypto/src/keys.rs:53-60`).
   Decide whether to keep those master-key-derived helpers or move them to a
   per-fleet KEK distributed via the same per-recipient wrap. Out of scope for
   M11; tracked as TIN-1417-followup.

## Touch Points

- `crates/tcfs-crypto/src/keys.rs` — add `wrap_for_recipients`,
  `unwrap_with_identity`, `WrappedFileKey` type.
- `crates/tcfs-crypto/src/recovery.rs` — unchanged for M11; the fixed-salt note
  at line 58 is acceptable because the mnemonic carries the entropy.
- `crates/tcfs-secrets/src/device.rs` — extend `DeviceIdentity` with
  `signing_pubkey`, `enrolled_by`, `revoked_at`; add keychain-backed secret
  storage trait; have `enroll` generate a real keypair (today only
  `enroll_remote` does, line 179).
- `crates/tcfs-sync/src/manifest.rs` — schema v3 with `wrapped_file_keys`
  field, preserve `encrypted_file_key` for backward compat.
- `crates/tcfs-sync/src/engine.rs` — replace the single `wrap_key` call site
  (line 1699) and the single `unwrap_key` call site (line 2055) with the
  per-recipient variants. Add recipient-set resolution at push time.
- `crates/tcfs-auth/src/enrollment.rs` — invite signing moves from
  `blake3::keyed_hash(master_key, ...)` (line 114) to Ed25519 over the
  inviter's signing key. Drop plaintext S3 secret broadcast from invites: an
  invite should reference an STS-issuable scope, not embed `storage_secret_key`
  (lines 49–51). This is TIN-1416 territory and the design here only asserts
  the requirement.
- `crates/tcfs-cli/src/main.rs` — `cmd_device_enroll` (line 2950) calls real
  keypair generation; `cmd_device_revoke` learns to publish a revocation
  intent to the remote registry and (optionally) trigger key rotation.
  `tcfs key rotate <prefix>` new command.
- `crates/tcfs-core/src/proto/tcfs.proto` — extend the manifest message in the
  gRPC service to carry the recipient list; bump service minor version.
- `crates/tcfs-file-provider/src/direct.rs` and `uniffi_bridge.rs` — hydration
  call sites (direct.rs:399, 519, 696; uniffi_bridge.rs:308, 405, 550) move
  to per-identity unwrap.
- `crates/tcfs-vfs/src/hydrate.rs:21` and `src/driver.rs:316` — same.

## Tests Needed

- `tcfs-crypto`: roundtrip property test that a FileKey wrapped to N recipients
  can be unwrapped by each of the N corresponding identities and by none of N
  random non-recipient identities.
- `tcfs-secrets`: enroll → revoke → check that the registry diff sent over S3
  is signed and that an unrelated device cannot mint a revocation record.
- `tcfs-sync`: push under per-device wrap, pull from device A succeeds, pull
  from device B (recipient) succeeds, pull from device C (non-recipient)
  returns a clear "no stanza for this identity" error rather than a corrupt
  decrypt.
- `tcfs-sync`: mixed-fleet integration test — one device on schema v2, one on
  v3, both pushing and pulling, both succeed during the Phase 1 window.
- `tests/e2e`: revocation flow — three-device fleet, revoke one, confirm the
  revoked device still decrypts old content (this is intentional!), cannot
  decrypt new content, and `tcfs key rotate` causes the rewritten files to
  also become opaque to the revoked device.
- `tcfs-auth`: invite minted by device A cannot be forged by device B; invite
  carrying STS scope reference (no plaintext secrets) round-trips through
  compact QR encoding under the QR size budget enforced by the existing
  `test_compact_deep_link` test.

## Open Questions / Decisions For The User

1. **age vs hpke vs Noise**: age X25519 is the recommendation because it is
   already in tree. Are we OK locking to it, or do you want a one-page
   evaluation of HPKE (RFC 9180) before committing? HPKE would buy us
   standardised KDF/AEAD agility but adds a dependency.
2. **Keychain coverage**: tcfs-secrets does not yet have a keychain abstraction
   for raw secrets (it handles SOPS/age decryption, not arbitrary blob
   storage). Phase 0 needs one. Should it be: (a) a new crate
   `tcfs-keychain`, (b) folded into `tcfs-secrets`, or (c) reuse an existing
   crate like `keyring-rs`? `keyring-rs` is the path of least resistance but
   does not cover the headless Linux daemon case cleanly.
3. **Revocation propagation transport**: the device registry today round-trips
   through S3 `meta_prefix/tcfs-meta/devices.json`
   (`tcfs-secrets/src/device.rs:147`). Revocations published there have S3
   eventual-consistency timing. Do we also publish revocations on NATS for
   sub-second propagation, or accept S3-only and document the propagation
   window?
4. **Forward secrecy default**: should `tcfs device revoke` automatically
   trigger `tcfs key rotate` on the device's accessible prefixes, or stay
   manual with a loud reminder? Automatic is safer; manual avoids surprise
   multi-GB re-uploads. Recommend manual + a `--rotate-keys` flag, but ask
   for a call.
5. **Multi-tenant boundary**: if TIN-1418 splits the fleet into tenants, is a
   "recipient set" scoped per-tenant, per-prefix, or per-file? Per-prefix is
   the natural fit (most files in a tenant share the same recipient set), but
   per-file gives the most flexibility for shared-but-restricted folders.
6. **Old PII in invites**: should we treat the existing plaintext-secret
   invites as a separate security incident worth a CHANGELOG note, or fold
   the fix silently into the TIN-1417 rollout? Recommend explicit note.
7. **Backward-compat window**: how long do we accept v2 single-wrap manifests
   on pull paths? One minor release? Two? Forever? Forever is safest for
   already-uploaded historical data; one minor is cleanest for code.

---

Review this document. Land alignment in the PR comments; implementation work
opens against TIN-1417 once the seven open questions have answers.
