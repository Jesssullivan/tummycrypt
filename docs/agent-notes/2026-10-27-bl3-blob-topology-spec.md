# 2026-10-27 BL-3 blob topology spec (Dhall)

Branch `docs/tin7498-bl3-layers-dhall-20261027` (off `origin/main`
`2d5b33e`). Spec, Justfile recipe and this note only; no Rust, Swift or Nix
logic changed. Draft PR, not for merge until Jess reviews it.
Rulings: BL2-Q9 and BL2-Q8 (Linear TIN-3692 comment `36925e2d`, Jess
2026-10-27 ~20:00Z); R-N13 (receipts, durable notes). Carrier: TIN-7498.

## What changed

- New `spec/` tree: Dhall types, the estate's component rows, six
  adjacent-layer interfaces, three replication legs, a file state machine,
  a workspace-root lease machine, rules, scenarios, 13 mutants and goldens.
  `spec/README.md` lists the files and rules.
- `Justfile`: `spec-dhall` and `spec-dhall-write`, pinned to the nixpkgs
  revision softconnect's spec uses.

## Verification

- `just spec-dhall` on sting, 2026-10-27: `spec-dhall: ok`; all 13 mutants
  rejected with their named rule. The pinned tools came from the binary
  cache; nothing was compiled.

## Findings recorded in the spec (evidence in the Linear document)

- `tcfs-core` `FileState` has no `FileState::` use in `crates/`;
  `tcfs-sync` `FileSyncStatus` is the live per-path vocabulary.
- `tcfs-storage` already requires create-if-absent and replace-if-match from
  the store (`verify_conditional_write_semantics`). SeaweedFS fixed several
  conditional-write cases after the live 4.48 (seaweedfs#11780, #11882,
  #12026), so the probe must run against the live endpoint before the
  `s3_hold` flip.
- NATS authority is unstated. `SYNC_TASKS` targets server-side worker pods,
  which would need content keys under client-side encryption.

## Open

- BL2-Q8 needs a design for worktrees and live SQLite. `Worktree.dhall` is a
  proposed contract only.
- Questions for Jess are in the Linear document, section 8.
