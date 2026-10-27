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

## Not done here

- The at-rest encryption code is a separate PR.
- No host, daemon, key or registry was touched.
