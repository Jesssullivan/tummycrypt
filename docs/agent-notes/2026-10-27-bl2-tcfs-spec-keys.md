# 2026-10-27 BL-2: v1 scope for worktrees and SQLite, device keys at rest

Branch `docs/bl2-tcfs-spec-keys-20261027` (off `origin/main` `2d5b33e`).
Rulings: Jess, Linear TIN-3692 comment `36925e2d` (BL2-Q8 "TCFS v1 must
cover them"; BL2-Q10/11 "Re-enroll + disable tcfsd"). Carrier: TIN-7498.
R-N13.

## What changed

- `docs/SPEC.md`: requirements T13 (linked worktrees) and T14 (live SQLite).
  New section "Worktrees and live SQLite": what "cover" means (consistent
  snapshot and rebuild for worktrees, lock-aware snapshot and whole-file
  restore for SQLite), 14 failure modes v1 must not have, and the tests
  that gate removing each blacklist entry. Acceptance stage 4 now says v1
  includes T13 and T14. The security gap list adds plaintext device keys.
- `docs/ops/tcfs-bulkload-boundary.md`: points the two "Not tcfs" rows at
  the v1 ruling.
- `README.md`: one sentence pointing at T13 and T14.
- `crates/tcfs-secrets/src/device.rs` module doc: device keys are the
  plaintext age identity at mode 0600. The old text said "platform keychain
  or an owner-only age-encrypted file", which the code never did
  (`save_device_secret_key` writes the raw identity).

## Device keys at rest (second PR, branch `feat/device-key-at-rest-20261027`)

- `tcfs-secrets::device`: `DeviceKeyWrap` (the first X25519 identity of an
  age identity file, such as a host's sops age key file),
  `save_device_secret_key(.., wrap)`, `load_device_secret_key`,
  `decode_device_secret`, `protect_device_secret_key(s)` and
  `device_key_at_rest`. Encrypted files are armored age. The migration
  writes a temporary sibling, reads it back, compares, then renames.
- Config: `crypto.device_key_wrap_identity_file`, overridden by
  `TCFS_DEVICE_KEY_WRAP_IDENTITY_FILE`. A set path that does not load is an
  error, never a plaintext fallback.
- Readers: CLI and tcfsd `build_encryption_context`, and the FileProvider
  `resolve_device_identity` (file and inlined `device_secret`).
- Writers: `tcfs init`, `device enroll`, placeholder repair, tcfsd
  auto-enroll. tcfsd encrypts plaintext keys at startup when a wrap is set.
- CLI: `tcfs device protect-keys [--dry-run]`; `tcfs device status` prints
  `key_at_rest`.

Follow-ups, not in this PR:

- The macOS host app inlines the raw `device-<id>.age` bytes as
  `device_secret`. Once a key is encrypted, the sandboxed extension cannot
  decrypt it and falls back to the master wrap. The host must inline the
  decrypted identity. Inert today: every host runs `wrap_mode = master`.
- lab sets `crypto.device_key_wrap_identity_file` per host when it bumps to
  a release with this code. Each host delivery needs its own GO (R-C754).

No host, daemon, key or registry was touched.
