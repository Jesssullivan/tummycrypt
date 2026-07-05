# Shared-Master to Per-Device Crypto Migration Plan (TIN-1417)

Status: operational migration plan. Doc-only. No code lands with this document;
no fleet config flips with this document. This is the verified, ratified
sequencing the fleet follows to move from the shared-master-key model to
per-device age/X25519 FileKey wrapping. It is an acceptance artifact for the
TIN-1417 design (`docs/ops/per-device-crypto-identity-design-2026-05-18.md`) and
should be read as the operator-facing companion to that design recon.

Date: 2026-06-06.

2026-07-05 truth update: the forward-secrecy rotation action described here is
not currently accepted as a usable implementation surface. Linear `TIN-2551`
supersedes the old `TIN-1899` completion claim for `tcfs key rotate <prefix>`:
the WIP rotation path was adversarially reviewed and found to have
revocation-defeating defects. Keep the expand/contract migration sequencing
below as the current per-device wrapping plan, but treat `tcfs key rotate
<prefix>` as a rebuild gate, not an available operator remedy, until `TIN-2551`
lands with fresh tests and review.

Grounding: every primitive, phase, and revocation claim here derives from
`docs/ops/per-device-crypto-identity-design-2026-05-18.md` (the "design doc"
below). Where this plan adds operational structure not in the design doc — an
expand/contract ordering, the tri-state `crypto.wrap_mode` gate, per-step
rollback — it is called out as such and grounded in the verified code state on
`origin/main` as of this date.

## The control surface: `crypto.wrap_mode` (tri-state)

The migration is driven by a single tri-state config enum,
`crypto.wrap_mode`, with three values. This REPLACES the earlier
shipped-but-never-flipped `crypto.per_device_wrapping: bool` (v0.12.14, default
false, set true NOWHERE live). The enum makes the expand/contract phases
explicit and impossible to conflate:

| `wrap_mode`  | Manifest carries                                  | Manifest version | Migration phase |
|--------------|---------------------------------------------------|------------------|-----------------|
| `master`     | `encrypted_file_key` only                         | v2               | baseline / default |
| `dual`       | BOTH `encrypted_file_key` AND `wrapped_file_keys` | v2               | EXPAND / transitional |
| `per_device` | `wrapped_file_keys` only (master wrap dropped)    | v3               | CONTRACT |

Semantics, exactly:

- **`master`** (DEFAULT) — today's master-only wrap. This MUST be byte-identical
  to the prior `per_device_wrapping=false` behavior: the engine takes
  `EncryptionContext::new(master_key)` and the legacy single-wrap path,
  unchanged. Manifests stay v2.
- **`dual`** (EXPAND) — every writer emits BOTH the legacy master wrap
  (`encrypted_file_key`, for rollback + master/old-binary readers) AND the
  per-device wraps (`wrapped_file_keys`). Any reader, upgraded or not, can
  decrypt: an upgraded reader uses its per-device wrap; a master-only / legacy
  reader falls back to the master wrap. Back-compatible by construction, so
  manifests stay v2 (design doc Phase 1, line 207).
- **`per_device`** (CONTRACT) — writers emit ONLY `wrapped_file_keys` and DROP
  the master wrap, so a device removed from the recipient set (revoked) cannot
  decrypt newly written content — true revocation. Because dropping the master
  wrap changes what an old reader sees, per-device manifests are bumped to
  **manifest version 3** so a pre-per-device binary fails CLOSED instead of
  misreading a v2-with-no-`encrypted_file_key` as keyless (design doc Phase 2/3,
  lines 212–219).

Going forward `wrap_mode` is canonical. For back-compat the deserializer still
accepts a legacy `per_device_wrapping` field and maps it: `true` -> `dual`
(safe — keeps the master fallback, never silently drops it), `false`/absent ->
`master`. When both keys are present, `wrap_mode` wins.

## Why expand/contract

The design doc's Migration Path (design doc lines 198–224) lays out Phase 0
through Phase 4 as a schema evolution. This plan re-expresses that as a strict
**expand-then-contract** discipline so that at no point is there a fleet state
where a healthy, non-revoked device cannot read content it is entitled to read:

- **Expand** = `wrap_mode = dual`: every writer emits BOTH wraps. Any reader,
  upgraded or not, can decrypt. This is design doc Phase 1 (line 207).
- **Contract** = `wrap_mode = per_device`: only after a green fleet roll-call do
  writers stop emitting the legacy master wrap, per manifest, one direction.
  This is design doc Phase 2/3 (lines 212–219).

`dual` is the EXPAND switch: it makes writers ADD per-device wraps WITHOUT
removing the master wrap. `per_device` is the CONTRACT switch: it is the only
mode that drops the master wrap, and it is the only mode that writes v3
manifests. Splitting expand (`dual`, v2) from contract (`per_device`, v3) is the
core safety property of this migration.

Default-off behavior MUST stay byte-identical: with `wrap_mode = master` (the
default) the engine takes `EncryptionContext::new(master_key)` and the legacy
single-wrap path unchanged (verified: daemon `build_encryption_context` returns
`base` early when the mode is `master`, `crates/tcfsd/src/grpc.rs`).

## The roll-call code gate (before `per_device`)

Contracting to `per_device` drops the master wrap, which strands any active
device that is NOT per-device-capable. This is a CODE gate, not a doc note: the
daemon (and CLI, and FileProvider read path) REFUSES to operate in `per_device`
until a roll-call probe confirms every active (non-revoked) device in the
registry has a real, parseable age recipient
(`tcfs_secrets::device::is_real_age_public_key`). Until that probe reads green,
the requested `per_device` mode is DOWNGRADED to `dual` and a loud warning is
emitted (`ROLL-CALL GATE: ... refusing to drop the master wrap. Falling back to
DUAL ...`) listing the incapable device IDs. It never silently drops the master
wrap.

Verified surface: `DeviceRegistry::per_device_roll_call_ready()` returns true
only when there is at least one active device AND every active device is
per-device-capable; `per_device_incapable_active_devices()` enumerates the ones
that are not. The daemon resolves the effective mode from the requested mode and
this probe, and attaches it to the `EncryptionContext` via
`.with_wrap_mode(...)`. The engine write path trusts that effective mode as
authoritative and does not re-run the probe — so any caller that wants
`per_device` but cannot prove fleet capability MUST have already downgraded to
`dual`. There is also a structural safety floor: if a non-`master` mode is
requested but there are zero per-device recipients (nobody to wrap for), the
write path falls CLOSED to master-only v2 output rather than emitting a keyless
or unreadable manifest.

## Migration Sequence

### Step 1 — B1 FileProvider parity (expand, prerequisite)

Bring the FileProvider direct restore path to crypto parity with the daemon and
CLI before any per-device wrapping is enabled anywhere on the fleet.

Verified gap (historical): the FileProvider direct read path built
`EncryptionContext::new(mk)` only — device identity was `None`, it never called
`.with_device_wrapping`. The engine read switch takes the `wrapped_file_keys`-
non-empty branch and (for a v3 per-device-only manifest) hard-fails with no
master fallback. So the moment any per-device-only manifest reached a
FileProvider hydrate, hydration broke. PR #492 closed this by adding an
FP-local `build_encryption_context` (`crates/tcfs-file-provider/src/device_ctx.rs`)
mirroring the daemon's fail-closed-to-master-readable logic; this plan updates
that helper to read `crypto.wrap_mode` (with legacy `per_device_wrapping`
back-compat) and apply the same roll-call gate.

Step 1 lands BEFORE any host moves off `master`, so it is a no-op while every
manifest is still legacy single-wrap. It only matters once Step 3 puts
per-device manifests on the wire.

Note: the non-prod default/uniffi backends only implement master-key
unwrapping. A per-device (`wrapped_file_keys`) manifest reaching them would copy
chunks as raw ciphertext (silent corruption); `ensure_master_decryptable`
(`device_ctx.rs`) fences these backends so a per-device manifest cannot reach
them and corrupt silently — it bails with a clear error instead.

### Step 2 — B3 dual-write through every caller + roll-call gate (expand)

Wire per-device dual-write through every restore caller and introduce the
roll-call gate in the shared `build_encryption_context` helpers.

The production restore callers must all honor per-device unwrap with master
fallback: auto-roam Pull (`crates/tcfs-sync/src/reconcile.rs`), the FileProvider
read path (`crates/tcfs-file-provider/src/grpc_backend.rs`, fixed in Step 1), and
CLI pull (`crates/tcfs-cli/src/main.rs`). Upload side: the single wrap call site
in the engine write path (`crates/tcfs-sync/src/engine.rs`).

In this step writers can run `wrap_mode = dual` (design doc Phase 1, line 207):
BOTH wraps emitted, v2. The roll-call probe (design doc line 216 — "gate the
flag on a fleet roll-call probe that confirms every active device is on the new
binary") is built here and must read green before Step 7. Crucially, the gate is
already wired so that even if an operator sets `per_device` early, the daemon
downgrades to `dual` until the roll-call is green — there is no unguarded path
that drops the master wrap.

### Step 3 — Canary both directions including FileProvider hydrate (expand)

Set `crypto.wrap_mode = dual` on a small canary subset (master wrap retained).
Exercise push from canary, pull on canary, pull on a legacy peer, and —
critically — FileProvider hydrate on macOS, which is the path Step 1 fixed and
the one most likely to regress. Verify every direction decrypts. Because the
master wrap is still present (dual is v2), a legacy or identity-missing reader
still succeeds via the master fallback in the engine read switch.

### Step 4 — B4 sign devices.json (expand)

Sign the device registry (`meta_prefix/tcfs-meta/devices.json`) so that
recipient sets and revocations cannot be forged by a peer. The design doc makes
the S3 registry the canonical revocation authority (design doc decision 3, lines
96–98) and requires that an unrelated device cannot mint a revocation record
(design doc Tests, line 264). Recipient-set integrity must be established before
`per_device` makes recipient-set membership the sole gate on readability.

### Step 5 — B2 per-device key rotation `tcfs key rotate <prefix>` (expand-capable)

Land the scoped rotation command (design doc lines 184–195, 250). This is the
ONLY mechanism that delivers forward secrecy after a revocation: it generates
fresh FileKeys, re-chunks and re-uploads under new BLAKE3 addresses, and
republishes manifests wrapped only to the post-revocation recipient set. It is
expensive (full re-push of the rotated set) and must surface projected
bytes-to-rewrite before the operator confirms (design doc line 194). Landing it
before the contract flip means revocation has a working remedy the day
`per_device` goes live.

### Step 6 — Keychain hardening (expand)

Move device secret halves into the OS keychain (macOS Keychain, Linux
secret-service / file-fallback, Windows DPAPI) per design doc lines 78–81 and
decision 2 (lines 92–95). The Phase 0 `0600` file fallback
(`device-<device_id>.age`) is acceptable for dev/test with an explicit
insecure-mode marker but is not the production posture. Harden before the
contract flip so a lost-laptop revocation is meaningful at the secret-storage
layer, not just the registry layer.

### Step 7 — Contract LAST: `per_device` after green roll-call (contract)

Only now, after Steps 1–6 and a green roll-call, set `crypto.wrap_mode =
per_device` so writers stop emitting the legacy master wrap and start writing v3
manifests (design doc Phase 2/3, lines 212–219). The roll-call code gate
described above is the enforcement: if any active device is not
per-device-capable, the daemon refuses to contract and stays on `dual` with a
loud warning — the operator setting `per_device` is necessary but not sufficient.

The contract is one-way PER MANIFEST: a manifest written `per_device` drops the
master wrap and bumps to v3 on its next version only; already-dual-written (v2)
manifests stay master-readable. Pull paths keep accepting all three shapes
indefinitely — v2 master-only, v2 dual, and v3 per-device-only (design doc
decision 7, lines 108–110, and Phase 3 line 217 — "keep the deserialiser"). The
engine read switch additionally fails CLOSED on a regular-file manifest whose
version exceeds the highest it understands (currently v3), so a future writer
cannot trick an older reader into misinterpreting its schema.

Master-key retirement (design doc Phase 4, lines 220–224) is explicitly OUT OF
SCOPE for this migration and tracked as TIN-1417-followup.

## Rollback (per step)

The migration is reversible at every step because expand never removes a wrap:

- **Steps 1–2 (code lands, mode stays `master`)**: revert the PR. No manifest on
  the wire changed; default behavior was byte-identical throughout.
- **Step 3 (canary, `wrap_mode = dual`)**: set `crypto.wrap_mode = master`.
  Writers revert to legacy single-wrap. Every manifest written during the canary
  is dual-written (v2, master wrap present) and therefore stays master-readable
  by every device — nothing is stranded.
- **Step 4 (signed registry)**: revert to unsigned; signing is additive
  verification, not a wrap change.
- **Step 5 (rotation)**: rotation is operator-initiated and idempotent at the
  prefix level; do not run it, or roll forward only the prefixes you intend.
- **Step 6 (keychain)**: file fallback remains the documented escape hatch.
- **Step 7 (`per_device`)**: set `crypto.wrap_mode = dual` (or `master`).
  Writers resume dual-write (or single-wrap) immediately. The contract is
  one-way PER MANIFEST and only takes effect after a green roll-call, so the
  blast radius of a bad flip is bounded to manifests written while `per_device`
  was on; those manifests (v3) are still readable by every then-active recipient
  (that is the roll-call precondition). Dual-written (v2) manifests from before
  the flip remain master-readable regardless. Note: reverting to `dual`/`master`
  does NOT rewrite the v3 manifests already on the wire — readers must still be
  able to unwrap them per-device, which the roll-call guaranteed before the flip.

Invariant: dual-written (v2) manifests stay master-readable; the contract
(master-wrap-drop, v3) transition is one-way per manifest and only after
roll-call.

## Honest Claim Boundary

Revocation denies NEW content only. This is the load-bearing honesty statement
from the design doc (Revocation Semantics, lines 164–195) and it does not change
under this plan:

- A revoked device CANNOT decrypt manifests written after the revocation
  propagates AND after those files are next rewritten/rekeyed in `per_device`
  (v3) mode.
- A revoked device CAN still decrypt anything it already pulled (it holds the
  FileKey forever — "there is no cryptographic time machine", design doc line
  180) and anything still carrying a wrap it can open.
- Critically: content that is already-pulled, OR still carries a master wrap
  (any v2 master/dual manifest), OR is not-yet-rekeyed, stays master-decryptable
  until a rebuilt-and-reviewed `tcfs key rotate <prefix>` re-chunks and rewraps
  it as `per_device` (v3). Revocation alone does NOT rewrite historical
  manifests (design doc lines 151–162). Forward secrecy is a SEPARATE, explicit,
  expensive operator action, and as of 2026-07-05 the accepted implementation is
  still gated on `TIN-2551`.

State this plainly in operator and CHANGELOG messaging: "Revoking a device stops
it from reading newly written content. It does not retroactively lock the device
out of content it already synced, and it does not lock it out of unchanged files
until a reviewed `tcfs key rotate` rebuild has rotated that prefix."

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
7. **Backward-compat window**: Keep v2 reads (master and dual) AND v3 reads
   (per-device) indefinitely; stop WRITING the master wrap only after the
   `per_device` contract gate (design doc decision 7).

## Residual Risks

- **Manual / partial forward secrecy**: revocation is not forward-secret by
  itself; the operator must run an accepted `tcfs key rotate <prefix>` per
  affected prefix, and any prefix not rotated stays master-decryptable (it is
  still a v2 master/dual manifest) to the revoked device. As of 2026-07-05 that
  rotation implementation is explicitly not accepted; track the rebuild in
  `TIN-2551`.
- **Deferred NATS advisory leg**: sub-second revocation propagation is out of
  scope; the fleet accepts S3 eventual-consistency timing and must document the
  propagation window.
- **Manifest version-3 namespace sharing**: regular-file per-device (v3)
  manifests and symlink (v3) manifests both use `version: 3`. They are
  disambiguated structurally — symlink manifests carry `kind: symlink` and are
  dispatched first via `SymlinkManifest::from_bytes` (which requires both
  `version == 3` and `kind == symlink`); a regular-file v3 manifest has no
  `kind` field and falls through to `SyncManifest::from_bytes`. This is verified
  but is a sharp edge: any future v3 regular-file schema change must preserve the
  no-`kind` discriminator so the dispatch stays unambiguous.
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
  symlink target. The merged symlink guard (#491) adds a deny-set / traversal /
  absolute-path check in `create_local_symlink`; per-device wrapping does NOT by
  itself mitigate a legitimate-but-compromised or hostile recipient, who is
  still a valid recipient. Tracked separately and must be fixed independently of
  this migration.

---

This plan is the operator-facing sequencing for TIN-1417. Land alignment in PR
comments. No mode flips and no fleet mutation occur from merging this doc.
