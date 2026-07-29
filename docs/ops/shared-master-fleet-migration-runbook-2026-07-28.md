# Shared-master fleet -> per-device: operator runbook (TIN-1417)

Status: operator runbook. Doc-only. **No config flips, no fleet mutation, and no
ceremony occur from merging this document.** It is the executable companion to
the ratified sequencing in
[`per-device-crypto-migration-2026-06-06.md`](per-device-crypto-migration-2026-06-06.md)
(the "plan doc"), which remains the authority for *why* the order is what it is.
This runbook adds what the plan doc deliberately left out: the exact operator
commands, the hard preconditions to check before each flip, what "green" looks
like, and where the abort is.

Date: 2026-07-28. Grounded in `origin/main` at the time of writing.

Reading order: design recon
([`per-device-crypto-identity-design-2026-05-18.md`](per-device-crypto-identity-design-2026-05-18.md))
-> plan doc -> this runbook.

## Who this is for

An operator running a fleet that is still on the shared-master-key model
(`crypto.wrap_mode = master`, the default) and wants to reach real per-device
wrapping. Every host in the fleet holds the same master key today; that is the
starting state this runbook assumes.

## The one-paragraph version

You cannot go straight to `per_device`. The only approved path is
`master -> dual -> (green roll-call) -> per_device`, and the FileProvider read
path must reach per-device parity **before** any host leaves `master`. `dual`
adds per-device wraps without removing the master wrap, so it is reversible;
`per_device` drops the master wrap and writes v3 manifests, so it strands anyone
who cannot unwrap per-device. Revocation only denies *new* content — it is not
retroactive, and it is not forward-secret without a separate, currently gated,
rotation.

## Status ledger (2026-07-28)

Where each step of the plan doc stands. This is the first thing to re-verify
before starting: do not trust this table, re-derive it.

| Plan-doc step | Substance | State |
|---|---|---|
| 1 — FileProvider parity | FP-local `build_encryption_context` + fail-closed fence | Landed (`crates/tcfs-file-provider/src/device_ctx.rs`). See P1 below for the residual trap. |
| 2 — dual-write through every caller + roll-call gate | `crypto.wrap_mode` tri-state, `DeviceRegistry::roll_call`, effective-mode downgrade | Landed (`crates/tcfs-core/src/config.rs`, `crates/tcfs-secrets/src/device.rs`). |
| 3 — canary both directions incl. FP hydrate | live neo/honey canary on a disposable prefix | **NOT DONE.** No fleet host has left `wrap_mode = master`. |
| 4 — sign `devices.json` | signed registry, unsigned-remote laundering refused | Landed AND executed on the live fleet 2026-07-09 (see `docs/release/evidence/ghost-device-revocation-2026-07-10T0107Z/`). |
| 5 — `tcfs key rotate <prefix>` | the only forward-secrecy remedy | **GATED.** The WIP path was found revocation-defeating; rebuild tracked in `TIN-2551`. Treat as unavailable. |
| 6 — keychain hardening | device secret halves out of `0600` files | Partial; file fallback is still the documented posture on headless Linux. |
| 7 — contract to `per_device` | writers drop the master wrap, manifests bump to v3 | **NOT DONE**, and blocked by steps 3 and 5. |

Consequence: the live fleet is at "code landed, mode still `master`". The next
action in the sequence is Step 3 (a disposable-prefix `dual` canary), not
anything involving `per_device`.

## Hard preconditions

These are pass/fail gates. Each one strands devices if skipped.

### P1 — FileProvider backend capability (the armed trap)

The historical trap was that the FileProvider direct read path built
`EncryptionContext::new(master_key)` with no device identity, so the first
per-device-only manifest to reach a hydrate would break it. That is closed for
the **gRPC** backend: `crates/tcfs-file-provider/src/device_ctx.rs` mirrors the
daemon's `build_encryption_context`, reads `crypto.wrap_mode`, applies the same
roll-call downgrade, and requires a signature-verified registry before it will
trust a recipient set. The macOS FileProvider extension ships with
`--features grpc` (see `.github/workflows/release.yml`), so macOS is covered.

What is **not** covered, and is the live precondition:

- The `direct` and `uniffi` backends only implement master-key unwrapping. The
  `uniffi` backend is the iOS client.
- `ensure_master_decryptable` (`device_ctx.rs`, mirrored in
  `uniffi_bridge.rs`) fences them: a manifest with non-empty `wrapped_file_keys`
  and no master `encrypted_file_key` is refused with a loud error instead of
  materializing raw ciphertext. This converts silent corruption into a hard
  failure — it does **not** make those backends able to read per-device content.
- Therefore: **any client on the `direct`/`uniffi` backend fails closed on a v3
  per-device manifest.** Under `dual` it is fine (the master wrap is still
  there). Under `per_device` it is locked out of every newly written file.

Gate: before Step 7, enumerate every client that reads the affected prefixes and
prove each one is on the gRPC backend (or accept, in writing, that it is being
cut off). The roll-call gate does **not** catch this — it checks registry
recipient capability, not which FileProvider backend a host is running.

### P2 — Signed registry everywhere

`per_device` makes recipient-set membership the sole gate on readability, so the
registry must be tamper-evident before the contract flip. Verify no host reports
an UNSIGNED registry, and that no path still needs `--accept-unsigned-remote`.

```bash
tcfs device enroll --sync-remote          # must succeed with NO unsigned warning
ssh <peer> 'tcfs device enroll --sync-remote'
```

If a host still needs `--accept-unsigned-remote`, P2 is failed: re-sign from a
master-key holder first, then re-verify. The escape hatch is scheduled for hard
removal (`TIN-1900`, 2026-09-01).

### P3 — Green roll-call

Every active (non-revoked) device must carry a real `age1...` recipient. Ghost
and placeholder entries block the flip permanently until revoked.

```bash
tcfs device list     # every 'active' row must show a real age1... public key
```

Anything showing `age1-device-<hash>` is a placeholder: repair it
(`tcfs device enroll --repair-placeholder`) or revoke it (below). The code gate
enforces this independently — if the roll-call is not green, a requested
`per_device` is **downgraded to `dual`** with a loud warning rather than
silently dropping the master wrap.

### P4 — A working forward-secrecy remedy

`tcfs key rotate <prefix>` is the only mechanism that re-keys content after a
revocation, and it is currently gated on `TIN-2551`. Until it lands with fresh
tests and review, a revocation on this fleet is "denies new content" only, with
no available remedy for already-written content. Do not flip to `per_device`
while telling anyone that revocation is forward-secret.

## Device revocation (operator UX)

Revocation is the fleet-hygiene operation that makes the roll-call satisfiable
(P3) and the security operation the whole migration exists for. It is **sticky
fleet-wide**: the registry merge only ever flips `revoked` false -> true, so a
stale peer can never resurrect a revoked device, and there is no un-revoke.

Preview first — a dry run mutates nothing, local or remote, and prints exactly
what the real run will print:

```bash
tcfs device revoke local-fileprovider-data --dry-run
```

It reports the resolved target (name, id, recipient), whether the device is
already revoked, whether it is *this* host, the post-revoke active/capable
counts, the remaining roll-call blockers, the effective `wrap_mode`, and where
propagation would publish.

Apply and propagate in one step:

```bash
tcfs device revoke local-fileprovider-data --sync-remote
```

Then converge each peer:

```bash
ssh <peer> 'tcfs device enroll --sync-remote'
tcfs device list && ssh <peer> 'tcfs device list'
```

Selector: name, full device id, or the 8-character id prefix that
`tcfs device list` prints. Duplicate names are real (the May FileProvider
bring-up lanes minted same-shaped ghost identities), so an ambiguous selector is
a hard error listing the candidates — it never silently picks the first match.
When an id is available the revoke is applied **by id**, so a same-named sibling
is never collaterally revoked.

Guardrails, and why each exists:

| Flag / behavior | Why |
|---|---|
| interactive confirmation by default | revocation is sticky and cannot be undone by a peer |
| `--yes` | required for non-interactive stdin (ssh, cron, CI) — a script can never silently consume the prompt |
| `--dry-run` | rehearse the exact target and roll-call impact before mutating |
| `--sync-remote` | without it the revocation is **LOCAL ONLY** and no peer ever learns about it; the command says so loudly |
| `--accept-unsigned-remote` | same B4 laundering guard as enroll; refuses to merge-then-re-sign an unsigned remote unless explicitly accepted |
| `--allow-self` | refuses by default to revoke the identity this host is enrolled as, because that strands this host |
| forward-secrecy warning | recipient-set removal does not re-key what the device already holds |
| `wrap_mode = master` note | under `master` a revoke has **no cryptographic effect** at all; it is registry hygiene plus a roll-call precondition |

Idempotence: re-running against an already-revoked device is a no-op locally and
still republishes with `--sync-remote`, which is how you finish a
half-propagated revocation.

## Sequence

Steps map 1:1 onto the plan doc. Steps 1, 2, 4 have landed; they are listed for
completeness and re-verification, not re-execution.

### Step 3 — `dual` canary on a disposable prefix

Preconditions: P1 (macOS FP on the gRPC backend), P2 (signed registry
everywhere), and a prefix you are willing to throw away.

1. Set `crypto.wrap_mode = dual` on the canary host only. Do **not** set it
   fleet-wide, and do not touch the default in `config/`.
2. Exercise, in both directions: push from the canary, pull on the canary, pull
   on a peer left at `master`, and FileProvider hydrate/evict/rehydrate/mutate on
   macOS. The FP leg is the one most likely to regress and the whole reason
   Step 1 exists.
3. Green means: every direction decrypts, and manifests written during the
   canary are v2 carrying BOTH `encrypted_file_key` and `wrapped_file_keys`.

Abort: set `crypto.wrap_mode = master`. Every manifest written during the canary
is dual (v2, master wrap present) and stays readable by every device — nothing
is stranded. This abort is complete and costs nothing.

### Step 5 — rotation rebuild (blocking, not yours to skip)

`per_device` without a working `tcfs key rotate` means a revocation has no
remedy. Wait for `TIN-2551`. Re-prove it on a disposable scoped prefix — dry-run
first for the projected bytes-to-rewrite, then execute — before it counts as
satisfied.

### Step 6 — keychain hardening

Move device secret halves off the `0600` `device-<device_id>.age` fallback where
the platform supports it. Headless Linux remains the known weak spot and stays
on the file fallback; record that as an accepted residual risk rather than
pretending it is closed.

### Step 7 — contract to `per_device`

Only after Steps 3, 5, 6 and P1–P4. Set `crypto.wrap_mode = per_device`. The
roll-call code gate is the enforcement, not this document: if any active device
lacks a real recipient, the daemon refuses to contract and stays on `dual` with
a loud warning naming the blockers.

Verify after the flip:

- New manifests are v3 and carry `wrapped_file_keys` with **no**
  `encrypted_file_key`.
- Every active device can still read newly written content.
- A revoked device cannot read content written after the revocation propagated.
- macOS FileProvider hydrate still works on the per-device path.

Abort: set `crypto.wrap_mode = dual` (or `master`). Writers resume dual-write
immediately. **The abort is partial**: v3 manifests already written are not
rewritten, so readers must still be able to unwrap them per-device — which is
exactly what the green roll-call guaranteed before the flip. Blast radius is
bounded to manifests written while `per_device` was on.

## Verification checklist

Run before each flip and bank the output.

```bash
tcfs device list                 # roll-call: every active row has a real age1... key
tcfs device status               # this host's identity and revoked state
tcfs config show                 # effective crypto.wrap_mode actually in force
```

Then, per flip: a push from the changed host, a pull on a peer, a pull on a host
left one mode behind, and — on macOS — a FileProvider hydrate. A flip is not
green until all four are.

## Evidence

Bank each step as a dated packet under
`docs/release/evidence/per-device-<step>-<UTC>/` containing the verbatim command
output for the checklist above plus the direction tests. The ghost-device
revocation packet (`docs/release/evidence/ghost-device-revocation-2026-07-10T0107Z/`)
is the shape to copy.

## Honest claim boundary

Unchanged from the plan doc, restated because it is the thing most likely to be
overclaimed in a status update:

- A revoked device **cannot** decrypt manifests written after the revocation
  propagates AND after those files are next rewritten in `per_device` (v3) mode.
- A revoked device **can** still decrypt anything it already pulled — it holds
  those FileKeys forever — and anything still carrying a wrap it can open.
- Content that is already-pulled, still carries a master wrap (any v2
  master/dual manifest), or is not-yet-rekeyed stays decryptable until a
  rebuilt-and-reviewed `tcfs key rotate <prefix>` re-chunks and rewraps it.
- Under `crypto.wrap_mode = master` — the current fleet state — revocation has
  **no cryptographic effect whatsoever**.

Operator-facing phrasing: "Revoking a device stops it from reading newly written
content. It does not retroactively lock the device out of content it already
synced, and it does not lock it out of unchanged files until a reviewed
`tcfs key rotate` has rotated that prefix."

## Residual risks

Inherited from the plan doc (manual forward secrecy, deferred NATS advisory leg,
v3 manifest namespace sharing, AES-SIV filename determinism, headless-Linux
keychain, invite authenticity, symlink restore). Added by this runbook:

- **Backend capability is invisible to the roll-call.** The roll-call proves
  registry recipient capability, not that a client's FileProvider backend can
  perform a per-device unwrap. An iOS/`uniffi` client passes the roll-call and
  still fails closed on v3. P1 is a manual gate with no code enforcement.
- **Propagation window.** A revoke is only fleet-canonical after a signed
  `--sync-remote` republish AND a merge on each peer. Half-propagated state is
  harmless under `master` (no crypto effect) but must be closed before any
  `dual` flip.
- **No un-revoke.** Sticky merge is deliberate; a mistaken revocation is
  repaired by enrolling a NEW identity, not by reversing the old one.
