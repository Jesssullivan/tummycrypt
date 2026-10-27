# Blob topology spec (Dhall)

Draft for Jess's review (TIN-7498, BL2-Q9 in Linear TIN-3692 comment
`36925e2d`). It states the layers of the estate's blob topology, where TCFS
sits, the contract between each pair of adjacent layers, and a small
functional model of one file and one workspace root across hosts. The prose
version is the Linear document "BL-3: blob topology layers and TCFS
interfaces (2026-10-27)".

The modelled contract is a target. Where the live estate differs, the gap is
written into the interface's `completeness` field with its owner.

## Files

| File | Holds |
| --- | --- |
| [dhall/Types.dhall](dhall/Types.dhall) | Layers, roles, concerns, commit points, the interface record |
| [dhall/Layers.dhall](dhall/Layers.dhall) | One row per component, with version, status, role and source |
| [dhall/Interfaces.dhall](dhall/Interfaces.dhall) | The 6 adjacent-layer interfaces and 3 replication legs |
| [dhall/File.dhall](dhall/File.dhall) | One path on one host: a total `step` over 7 states and 13 events |
| [dhall/Worktree.dhall](dhall/Worktree.dhall) | One workspace root across 7 hosts: the writer lease and its handoff |
| [dhall/Check.dhall](dhall/Check.dhall) | Rules L1-L4, I1-I5, R1, F1-F6, W2-W5; each returns the broken rules |
| [dhall/Model.dhall](dhall/Model.dhall) | Asserts the rules on both machines and runs the core user story |
| [dhall/mutations/](dhall/mutations/) | 13 wrong variants; each must fail with the rule its header names |
| [dhall/generated/](dhall/generated/) | Goldens: layers, interfaces and the file transition table |

## Run it

```sh
just spec-dhall        # type-check, compare goldens, require every mutant to fail
just spec-dhall-write  # refresh the goldens after a source change
```

`spec/check-dhall.sh` pins dhall 1.42.3, dhall-json 1.7.12 and jq 1.8.2, and
refuses imports from the network or the environment. It follows the
softconnect and agent-switchboard spec checks.

## Rules

| Id | Rule |
| --- | --- |
| L1 | Every layer has a live, paused or ruled component that a product owns, holds in the interim or leverages |
| L2 | One concern has one owner; a second claimant is Interim until it hands over |
| L3 | An aspirational component stays proposed or absent until the core user story is demonstrated |
| L4 | The TCFS namespace commits by a conditional write on the object store and adds no metadata server |
| I1 | Each pair of adjacent layers has exactly one interface |
| I2 | The object store gives the namespace strong reads and both create-if-absent and replace-if-match |
| I3 | A specified interface states its consistency and every failure mode; gaps are Partial or Open with an owner |
| I4 | An interface that crosses hosts is encrypted in transit |
| I5 | The namespace tells the sync engine that absence is not deletion |
| R1 | A replication leg states its lag bound and is encrypted in transit |
| F1 | A missing index entry is not deletion authority; only a tombstone is |
| F2 | Bytes that exist only on this host leave that state only by a won publish |
| F3 | Unsync turns a clean or hydrating file back into a stub |
| F4 | A lost compare-and-swap never yields a clean file; the loser keeps both |
| F5 | An unreachable store ends every in-flight operation in a resting state |
| F6 | Names appear before content, and a read hydrates |
| W2 | A writer lease moves only through a handoff |
| W3 | Diverged writes leave only through reconciliation |
| W4 | A handoff completes to its target only when quiesced and falls back to its source only when quiescing fails |
| W5 | A root retires only when no host holds or contests its lease |

W1, one writer at a time, is not a check: `Active` carries exactly one host,
so a second writer cannot be written down except as `Diverged`.
