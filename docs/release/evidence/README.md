# TCFS evidence ledger

This directory contains immutable proof and blocker packets. The index promotes
the strongest current packets; it does not make every archived run current.
Read [the living workstream](../../ops/current.md) for today's blockers and
[the product sequence](../../PRODUCT.md) for the acceptance ladder.

## Claim rules

- A packet proves only the hosts, binaries, paths, direction, client, and
  lifecycle named inside it.
- A blocker or partial packet remains evidence and never becomes a passing
  claim because a later adjacent run passed.
- Source-built, packaged, dry-run, readiness, and live-fleet results are
  distinct proof classes.
- Packet contents are not rewritten to match later conclusions. Supersession
  belongs in this ledger or in a newer packet.

## Current promoted proofs

| Date | Packet | Promoted claim | Boundary |
| --- | --- | --- | --- |
| 2026-07-10 | [Ghost-device revocation](ghost-device-revocation-2026-07-10T0107Z/README.md) | Three known FileProvider ghost identities are signed-revoked on neo, remote registry, and honey | Registry hygiene under `wrap_mode=master`; no re-key or per-device-only crypto proof |
| 2026-07-08 | [Divergent keep-both canary](divergent-keep-both-canary-20260707T071335Z/RESULTS.md) | Two divergent Git heads converge with both commits reachable, fsck clean, parked loser ref, verified undo bundle, and a clean second cycle | Automatic loser-side guard; the operator `resolve` verb is not proven |
| 2026-07-05 | [Bidirectional fast-forward canary](bidirectional-ff-canary-20260705T225429Z/RESULTS.md) | Commit on either neo or honey and fast-forward the other host with zero conflict | Fast-forward ancestry only; not divergent keep-both |
| 2026-07-02 | [No-crutch repo roam](repo-roam-nocrutch-v0.12.15-20260702T052331Z/RESULTS.md) | Full forward dev-environment roam restores correct Git status without manual index refresh | One repo, neo → honey, selected binaries |
| 2026-06-09 | [Repo-roam zero-diff canary](repo-roam-canary-20260609/RESULTS.md) | Branch, staged and unstaged edits, untracked file, stash, and history arrive byte/semantic exact with fsck clean | Forward direction; its original reverse expected-fail is superseded by the two July packets above |
| 2026-05-18 | [Production Developer ID FileProvider lifecycle](macos-postinstall-prod-devid-hydration-20260518T212705Z/) | Signed macOS lab lane covers enumerate, hydrate, evict/rehydrate, mutation/readback, and conflict-status preservation | PZM lab lifecycle; not polished first run or continuous per-release production proof |
| 2026-05-14 | [Large project-tree lifecycle](home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/) | 7.7 GB isolated Linux tree supports traversal, exact hydration, 85 symlink targets, write/readback, rehydrate, and safe unsync | Plaintext HTTP endpoint and isolated shadow; not production storage posture or home takeover |
| 2026-05-11 | [Scoped project-tree parity](home-canary-linux-xr-shadow-20260511T040325Z/) | Isolated `linux-xr` shadow passes bounded Linux traversal/hydration, symlink, and lifecycle checks | Shadow, not the live repo or Finder |
| 2026-05-10 | [Forward unsync/rehydrate](neo-honey-unsynced-rehydrate-20260510T015644Z/README.md) | Neo can unsync, honey can mutate, and neo can rehydrate exact current bytes | Disposable fixture |
| 2026-05-10 | [Reverse unsync/rehydrate](neo-honey-reverse-unsynced-rehydrate-20260510T022858Z/) | Honey can unsync, neo can mutate, and honey can rehydrate exact current bytes | Disposable fixture |
| 2026-05-08 | [Linux FUSE lifecycle](lazy-linux-20260508T170825Z/) | Browse-before-hydration, exact read, mounted write/readback, rehydrate, and safe recursive unsync | Honey/Linux FUSE |

Together, the July fast-forward and divergent packets close G5-git-5 for
automatic Git convergence. They do not close TIN-2658's authenticated,
root-targeted operator resolver.

## Packaging and release-era evidence

The repository's newest complete release matrix is still the historical
[`v0.12.13` matrix](../v0.12.13-evidence-matrix.md). It must not be presented
as a `v0.12.17` matrix.

Representative package packets:

| Packet | What it proves | Open boundary |
| --- | --- | --- |
| [v0.12.12 distribution](distribution-v01212-20260508T205913Z/) | Homebrew and tagged Darwin Nix install/upgrade at that release | Homebrew remains stale and skips symlinks |
| [v0.12.12 Linux packages](linux-packages-v01212-20260509T0231Z/) | Ubuntu/Debian packages and Fedora 42 daemon-only RPM | Rocky 10/FUSE and current-release re-proof |
| [v0.12.12 container refresh](container-v01212-manifest-refresh-20260514T224746Z/) | Registry manifest inspection | No native arm64 runtime proof |
| [Current-checkout Nix symlink probe](tcfs-symlink-package-probe-20260515T060330Z/README.md) | Selected Nix producer and consumer preserve/mount a symlink fixture | Not a tagged-current or Homebrew proof |

Until a `v0.12.17` matrix exists, release-tag presence, artifact build, install,
first-use, client lifecycle, upgrade, and rollback must be stated separately.

## Important blocker packets

These remain useful because they define known failure boundaries:

- [Daemon keep-both timeout](neo-honey-conflict-daemon-keep-both-20260510T054611Z/)
  — partial side effects under auth bypass; not clean resolver UX.
- [Neo mounted reverse-read blocker](neo-mounted-reverse-read-20260510T035826Z/)
  — macOS NFS loopback mount failed before mounted read.
- [Large Git restore blocker](git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/)
  — two multi-GB pack files failed after 2,036 of 2,038 regular files restored.
- [Homebrew symlink divergence](tcfs-symlink-package-probe-20260515T041947Z/README.md)
  — Homebrew skipped the link while current Nix/source builds preserved it.
- [Plaintext storage posture](home-canary-linux-xr-storage-posture-20260514T021513Z/)
  — functional large-tree evidence collected over HTTP, not a production TLS
  acceptance.

## Historical packets

Every directory remains part of the audit trail. Use its `README.md`,
`RESULTS.md`, `result.env`, or raw transcript to determine whether it is a
pass, blocker, superseded attempt, or plan-only packet. Git history retains the
former exhaustive May 2026 table at commit
`21f8df303596d1b9f6f90cc7953eb8f65f353ac3`.

Do not delete or edit packet contents as documentation cleanup. Add a new packet
or update this ledger when a stronger proof lands.

## Packets after 2026-05-31

Later proof records increasingly live in Linear rather than this tree:

- The forward-roam zero-diff `neo → honey` proof (2026-06-09) and the G5
  divergent keep-both closure (2026-07-08) are recorded under
  [TIN-1620](https://linear.app/tinyland/issue/TIN-1620).
- The TIN-2658 production-loop work (2026-07-14) is recorded on
  [TIN-2658](https://linear.app/tinyland/issue/TIN-2658).
- Untagged v0.12.15 ([#514](https://github.com/Jesssullivan/tummycrypt/pull/514))
  and v0.12.16 ([#522](https://github.com/Jesssullivan/tummycrypt/pull/522))
  shipped roam-critical work that is not visible in `gh release list`.
