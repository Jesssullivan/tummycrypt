# TCFS Lazy Traversal QA Permutation Matrix - 2026-05-09

This matrix turns the parity proof work into QA scenarios. It is not a claim
that every row is green today. It records the behavior TCFS needs to prove
across CLI sync roots, Linux mounted views, macOS Finder/FileProvider, and
multi-machine neo/honey flows.

## Representation Model

| Surface | Local representation | User expectation |
| --- | --- | --- |
| CLI physical sync root | Hydrated files plus `.tc` / `.tcf` stubs after `tcfs unsync` | `tcfs pull`, `tcfs unsync`, and `tcfs sync-status` preserve exact content and state. Raw `.tc` names are acceptable here. |
| Linux mounted VFS | Clean names backed by remote index and local cache | `ls`, `find`, and `cat` work on clean names before all bodies are downloaded. Raw `.tc` names should not be the primary mounted UX. |
| macOS FileProvider/Finder | Finder placeholders / APFS dataless files | Finder enumerates normal names, opens hydrate content, evicts/dehydrates through FileProvider, and records reliable logs/status. |
| Multi-machine fleet | Shared remote prefix plus per-device state/cache | One machine can dehydrate or remain unsynced while another edits, and the first machine can rehydrate exact latest content without stale local placeholders. |

## State Vocabulary

| QA term | TCFS term today | Notes |
| --- | --- | --- |
| Browse before download | Remote-index traversal | Linux VFS and FileProvider should show clean names without hydrating file bodies. |
| Open on demand | Hydrate | `cat`, Finder open, or requestDownload reads exact bytes and fills cache/provider content. |
| Remove from this machine | `unsync`, cache clear, or FileProvider evict | Physical sync roots use `.tc`; mounted VFS uses cache eviction/clear; Finder uses FileProvider eviction. |
| Keep synced | Pinned or continuously hydrated local copy | Product semantics are not fully accepted as a separate pin command yet; QA should mark this open unless a surface has an explicit pin/keep-local affordance. |
| Resync | Pull or re-open after dehydration | Must prove exact content, state transition, and stale-placeholder cleanup. |
| Conflict | Concurrent divergent writes | CLI/status behavior exists; Finder conflict UX remains experimental and should not be a production release gate yet. |

## Core Permutations

| ID | Scenario | CLI physical root | Linux mounted VFS | macOS FileProvider | Current coverage |
| --- | --- | --- | --- | --- | --- |
| T1 | List top-level tree before hydrating file bodies | Stub files can be inventoried as `.tc`/`.tcf` | `find` / `ls` show clean names from remote index | CloudStorage/Finder enumerates placeholders | Linux and PZM testing-mode green; production Finder open |
| T2 | Traverse nested directory before hydrating children | Directory stubs or tracked state enumerate physical representation | Nested `find` / `ls` shows clean names | Finder expands nested folders | Linux VFS tests and host evidence green; production Finder open |
| T3 | `cat` selected file on demand | `tcfs pull` or stub-aware path restores exact file | `cat clean/name.txt` hydrates cache and returns exact bytes | Finder open/requestDownload hydrates exact bytes | Linux and PZM testing-mode green |
| T4 | Re-`cat` after cache clear or eviction | Pull again from remote | Clear VFS cache or `unsync_path`, then `cat` exact bytes | Evict, then request/open exact bytes | Linux and PZM testing-mode green; mounted `unsync_path` eviction/rehydrate regression covers the VFS primitive |
| T5 | Unsync a clean file | `tcfs unsync file` writes valid `.tc`, removes hydrated file, state becomes `NotSynced` | `unsync_path` evicts cached bytes while preserving clean-name traversal | FileProvider evict equivalent | CLI tests green; mounted VFS cache-evict primitive regression green |
| T6 | Unsync a clean directory recursively | Clean tracked descendants become `.tc`; empty dirs and state preserved as applicable | Mounted equivalent is cache clear/eviction | Finder equivalent is recursive eviction if supported | CLI/Linux lifecycle green for safe-unsync |
| T7 | Refuse dirty unsync | `tcfs unsync dir` refuses dirty descendant unless `--force` | Mounted writes must not be discarded silently | Finder dirty local content must not be evicted destructively | CLI/Linux lifecycle green; Finder open |
| T8 | Force unsync dirty file | `tcfs unsync --force` preserves tracked remote metadata and writes stub | Not primary mounted UX | Finder force-remove semantics not accepted | CLI regression green |
| T9 | Pull to clean path while adjacent stub exists | Pull writes exact content and removes adjacent parseable TCFS `.tc` stub | Not applicable | Finder placeholder replaced by hydrated file content | Added CLI regression in this sprint |
| T10 | Do not delete unrelated adjacent `.tc` sidecar | Pull ignores non-TCFS text/binary sidecar named `<file>.tc` | Not applicable | Not applicable | Added CLI guard/regression in this sprint |

## Cross-Machine Permutations

| ID | Scenario | Required proof | Current route |
| --- | --- | --- | --- |
| M1 | neo pushes, honey lists before hydration | honey mounted `find` / `ls` shows clean names and does not hydrate bodies unnecessarily | `task lazy:fleet-pilot-plan` with `RUN_HONEY=1` |
| M2 | neo pushes, honey cats one file | honey `cat` hydrates exact selected content only | `scripts/lazy-hydration-mounted-smoke.sh` through fleet helper |
| M3 | neo pushes, neo unsyncs, honey edits mounted file, neo pulls same relative path | neo receives honey bytes, stale `.tc` disappears, `sync-status` is `Synced` | Green: `docs/release/evidence/neo-honey-unsynced-rehydrate-20260510T015644Z/`, helper regression, and CLI regression |
| M4 | honey pushes, neo unsyncs, neo re-cats through mounted view | Clean-name mounted read on neo hydrates latest honey bytes | Blocked live on neo in `neo-mounted-reverse-read-20260510T035826Z/`: honey push and neo physical unsync passed, but neo NFS loopback mount failed with `Operation not permitted` before mounted `cat` |
| M4-L | neo pushes, honey unsyncs, honey re-cats through mounted Linux view | Clean-name mounted read on Linux hydrates latest neo bytes while the physical root remains stub-only | Green: `docs/release/evidence/honey-mounted-reverse-read-20260510T042203Z/`; this is the Linux-equivalent mounted VFS proof, not a neo/macOS or production Finder closure |
| M5 | honey edits while neo has hydrated clean copy, then neo also edits/pushes | honey push detects conflict instead of overwriting neo, honey local bytes are preserved, and remote pullback still has neo bytes | Green for current CLI behavior in `docs/release/evidence/neo-honey-conflict-20260510T043741Z/`; this is conflict detection/preservation, not resolution UX |
| M5-R | after M5 conflict, manually keep both versions | original path rehydrates to winning remote bytes, losing peer bytes are preserved under a sibling path, and both paths pull back with exact hashes | Green for manual current behavior in `docs/release/evidence/neo-honey-conflict-keep-both-20260510T045908Z/`; this is a scriptable recovery pattern, not daemon-backed `tcfs resolve` or Finder conflict UX |
| M5-D | after M5 conflict, run daemon-backed `tcfs resolve --strategy keep-both` | daemon returns cleanly, original path has winning remote bytes, losing peer bytes are preserved under daemon conflict-copy path, state is synced, and both paths pull back with exact hashes | Blocked in `docs/release/evidence/neo-honey-conflict-daemon-keep-both-20260510T054611Z/`: isolated honey `tcfsd 0.12.12` accepted the request under auth bypass, but the CLI RPC timed out after 30s. Post-timeout pullbacks show partial side effects, not clean resolution UX |
| M6 | neo edits while honey has unsynced/evicted copy | honey re-open/pull sees neo bytes and no stale placeholder | Green: `docs/release/evidence/neo-honey-reverse-unsynced-rehydrate-20260510T022858Z/`, after stale-binary blocker `20260510T022657Z` |
| M7 | both machines edit offline/unsynced descendants | Conflict state is visible, exact local content is preserved until resolved, and unrelated descendants can continue syncing | Partially green: M5 cross-host same-file conflict packet plus CLI/PZM conflict tests cover core preservation; `neo-honey-conflict-sibling-20260510T051328Z/` proves an independent sibling descendant can sync while another descendant remains conflicted. Full descendant/offline conflict matrix and resolution UX remain open |
| M8 | delete/rename on one machine while other is unsynced | Remote index, trash/delete semantics, and stale local placeholder cleanup are deterministic | Green for current behavior in `neo-honey-delete-rename-unsynced-20260510T040456Z/`: old paths fail, renamed new path hydrates exact bytes, and stale old stubs are recorded as an open tombstone/cleanup gap. Helper coverage now also records stale-stub `sync-status`, repeated old-path pull failure, and repeated new-path hydrate success for future packets. Lower-level regressions cover unhydrated mounted delete/rename and CLI delete-then-recreate over an unsynced peer stub |

## Finder-Specific Permutations

| ID | Scenario | Production gate | Current status |
| --- | --- | --- | --- |
| F1 | Install published `.pkg` to `/Applications/TCFSProvider.app` | Developer ID package install succeeds on clean host | Local neo install blocked by non-interactive sudo; hosted attempt passed install before storage failure |
| F2 | Strict signing/profile preflight | `TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` green | Existing neo user app fails strict preflight; source-built Developer ID app now passes signing-only strict preflight with embedded profiles in `macos-fileprovider-signed-app-preflight-20260516T183213Z/`, but it is not package-installed or registered |
| F3 | Finder enumerate | CloudStorage root appears and lists placeholders through FileProvider | PZM testing-mode green; production clean-host open |
| F4 | Finder open/requestDownload exact hydrate | Exact bytes through coordinated read | PZM testing-mode green; production clean-host open |
| F5 | Finder evict and rehydrate | Evict clears local body, re-open hydrates exact bytes | PZM testing-mode green; production clean-host open |
| F6 | Finder mutation upload/readback | Edited CloudStorage bytes upload and remote pull matches | PZM testing-mode green; production support claim still experimental |
| F7 | Finder conflict/status visibility | CLI conflict status, exact FileProvider content preservation, and reliable UI/log signal | PZM content preservation green; badge/progress UX observational only |

## Project-Tree Permutations

| ID | Scenario | Required proof | Current status |
| --- | --- | --- | --- |
| P1 | Shadow real repo without mutating source | Source inventory, full isolated shadow, config/state under shadow | Green for isolation: `home-canary-linux-xr-shadow-20260511T040325Z` inventories the source read-only and uses an isolated shadow; live source was not mutated |
| P2 | Include `.git` and hidden dirs | `sync_git_dirs = true`, `sync_hidden_dirs = true`, `git_sync_mode = "raw"` | Green in `20260511T040325Z`: raw `.git`/hidden-dir config, completed push, honey mounted traversal at `max-depth=8`, selected hydrate, and Linux lifecycle passed |
| P3 | Symlink truth gate | Inventory symlinks and preserve them as symlinks with matching targets, or keep full parity blocked | Green in `20260511T040325Z` for the scoped isolated-shadow canary: source/shadow manifests matched, 85 symlink uploads were present, and honey mounted `readlink` verification passed for all 85 symlinks |
| P4 | Unsupported special files | Inventory sockets/FIFOs/devices/permissions and record behavior | Green inventory: source and shadow record `unsupported_special_files=0` |
| P5 | Large tree scale | Push/honey traversal/hydration completes without source mutation | Green scoped packet: `20260511T040325Z` push completed, honey mounted `find -maxdepth 8`/hydrate passed, all 85 mounted symlink targets matched, and the Linux lifecycle companion passed. It remains functional evidence, not production S3 posture |

## Storage / S3 Performance Permutations

Correctness packets and storage posture packets answer different questions. A
run can prove exact content and traversal while still revealing S3 object-count,
queueing, retry, or endpoint problems that block production claims.

| ID | Scenario | Required proof | Current status |
| --- | --- | --- | --- |
| S1 | Large raw-Git index push | `.git/objects/pack/*.idx` uses a large-file chunk profile, records chunk count and push duration, and does not explode into tiny S3 objects | `20260510T201809Z` exposed the old small-profile behavior: a 395,849,892-byte `.idx` became 72,598 chunks. `20260511T040325Z` records the improved moderate-profile shape at 4,600 total pack-index chunks. The later `linux-xr-fast` package blocker shows that is still too much for raw `.git` stress; source now routes Git pack indexes to the large sequential profile, but candidate-package storage proof remains open |
| S2 | Large raw-Git pack push | Multi-GB `.pack` files push without unbounded memory growth, with archived chunk count, bytes uploaded, retry count, and wall-clock duration | `20260511T040325Z` records the pre-fix dominant pack shape: max object `pack-cca8376c...pack` at 70,856 chunks / 6,216,046,897 bytes. `home-canary-linux-xr-storage-posture-20260513T220442Z` and `20260514T021513Z` reran with the large sequential profile and completed that same `.pack` as 1,211 chunks with no retry or error rows; `home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z` adds same-prefix lifecycle parity. Production posture remains blocked on endpoint/TLS, socket accounting, candidate-package proof for Git pack indexes, and generated-large-file policy. |
| S3 | Endpoint posture | Packet records endpoint class, TLS policy, credential source, bucket/prefix isolation, and whether public CI can reach it safely | Open. Existing packets use disposable prefixes, but production-like endpoint class and public-runner reachability remain separate proof rows. `task lazy:home-canary-linux-xr-storage-posture` now records endpoint/TLS posture and credential presence without secret values for the next packet |
| S4 | Queue/concurrency behavior | Upload engine records whether file and chunk writes are sequential or bounded-parallel, per-chunk write timeouts are explicit, and transport/timeout retries are visible in evidence | `20260513T174944Z` records the post-PR #367 rerun with `file_upload_concurrency=8`, `chunk_upload_concurrency=8`, 47 warning rows, 43 retry warning rows, 31 timeout retry warning rows, socket high-water samples, and fresh-prefix skip telemetry. `20260514T021513Z` completed with the same concurrency settings, no retry or error rows, and socket highwater 11 while upload concurrency was 8. This proves observability and bounded upload knobs, not production socket accounting or storage readiness. |
| S5 | Hydration latency on S3 | Cold list, first-byte hydrate, full hydrate, cache-hit read, and cache-clear/rehydrate timings are archived for representative small and large files | Open. Current traversal rows prove exact bytes, not latency SLOs |

## QA Evidence Minimums

Every live QA packet should archive:

- disposable remote prefix, endpoint, bucket, and run ID
- source root and proof that it is isolated from real `~/Documents`, `~/git`, dotfiles, and broad home takeover
- config and state paths
- tree inventory before hydration
- exact content fixtures and hashes before/after hydrate, mutate, unsync, and rehydrate
- command transcript for `ls`/`find`, `cat`, `tcfs pull`, `tcfs unsync`, and `tcfs sync-status`
- stale placeholder/stub checks after rehydrate
- storage metrics for S3-backed packets: endpoint class, TLS policy, object/chunk counts, selected chunk profile, bytes uploaded/skipped, retry counts, queue/concurrency settings, and wall-clock duration for large objects
- platform signing/profile/build details for FileProvider runs
- blocker notes when a row is not claimed, especially symlinks, tombstones, keep-synced/pin semantics, and production Finder signing

## Claimability Bars

These are the explicit bars before QA or release notes can turn desirable
behavior into user-facing claims:

| Claim | Claimable only after |
| --- | --- |
| Production Finder | Published `.pkg` is installed into `/Applications/TCFSProvider.app`, stale PlugInKit registrations are removed or quarantined after inventory, `TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` is green, and CloudStorage/Finder enumerate, open, evict/rehydrate, mutate, and conflict/status evidence is archived under the production Developer ID label. |
| Scoped `linux-xr` isolated-shadow project-tree parity | The isolated shadow canary completes push, traversal, selected hydrate, write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean recursive unsync, exact rehydrate, and proves all inventoried symlinks rehydrate as symlinks with matching targets. Claimable for `docs/release/evidence/home-canary-linux-xr-shadow-20260511T040325Z/` only; this is not production Finder, broad home-directory takeover, or production S3 posture. |
| M4 mounted reverse read | Honey-originated bytes are visible from a neo mounted clean-name surface after neo was unsynced/evicted, with `ls`/`find`, `cat`, exact hashes, physical-stub state, and cache/state transcripts archived. The neo/macOS row is still blocked at mount permission. The Linux-equivalent mounted VFS row is green in `honey-mounted-reverse-read-20260510T042203Z/`, proving the behavior on honey but not production Finder. |
| M8 delete/rename while peer-unsynced | Old deleted/renamed paths fail deterministically, the new rename target hydrates exact bytes, and product semantics decide whether stale physical `.tc` placeholders are tombstoned, removed, or intentionally retained with status. The live packet proves current behavior, not clean stale-stub UX. |
| Cross-host conflict UX | Conflict detection/preservation is archived in `neo-honey-conflict-20260510T043741Z/`, manual keep-both recovery is archived in `neo-honey-conflict-keep-both-20260510T045908Z/`, independent sibling progress is archived in `neo-honey-conflict-sibling-20260510T051328Z/`, and daemon-backed keep-both is archived as a timeout/partial-side-effect blocker in `neo-honey-conflict-daemon-keep-both-20260510T054611Z/`; clean user-facing claim still needs conflict list/status, a returning daemon-backed `tcfs resolve`, and Finder/provider visibility where applicable. |
| Keep synced / pin | A product-level pin/keep-local affordance exists with status reporting, local storage guarantees, eviction rules, and conflict behavior. Until then, "keep synced" remains a planning term, not a proven QA row. |
| S3 production storage posture | Representative packets prove correctness plus acceptable endpoint, TLS, credential, object-count, retry, queue/concurrency, large-object push, and hydration latency behavior. Raw correctness packets are supporting evidence only. |

## Current High-Value Next Rows

1. Keep `linux-xr` scoped to the isolated shadow. `20260511T040325Z` is green
   for push, mounted traversal/hydration, all 85 mounted symlink `readlink`
   checks, and Linux lifecycle. Do not broaden it into production Finder, broad
   home-directory takeover, or production S3 readiness.
2. Decide tombstone/stale-stub semantics before making any clean delete/rename
   UX claim; M8 current behavior is live-proven, but stale old stubs remain.
3. Keep the neo/macOS M4 mounted reverse-read row open until a permitted mount
   path exists; use the Linux-equivalent M4-L packet as mounted VFS evidence,
   not as production Finder evidence.
4. Fix daemon-backed `tcfs resolve` completion: the current bounded packet
   reaches the daemon and records partial side effects, but the RPC times out.
   After that, extend into broader descendant/offline permutations and
   Finder/provider visibility.
5. Keep Finder rows under PZM testing-mode or production Developer ID labels;
   do not mix those evidence classes.
6. Treat S3 posture as its own proof lane. The raw-Git canary should keep
   exposing object-count and throughput behavior, but do not let exact-content
   success imply production storage readiness. The next host proof is to rerun
   large `.idx`/`.pack` paths with the rebuilt chunk profiles, streaming-memory,
   and bounded chunk-upload fanout changes via
   `task lazy:home-canary-linux-xr-storage-posture`; only then decide whether
   multipart or native SeaweedFS semantics are still required.

## Next Mutation Detail

These rows are the planning targets for the next QA packets and should not be
reported as product claims until evidence lands:

| ID | Mutation | Acceptance notes |
| --- | --- | --- |
| S6 | Post-fix release-binary raw-Git canary on a new disposable S3 prefix | `20260514T021513Z` archives release build provenance, endpoint/TLS/credential posture, `chunk_exists_check=false`, fresh-prefix publish mode, remote conflict-check skip, `chunk_write_timeout_secs=300`, `file_upload_concurrency=8`, socket samples, heartbeat/progress rows, completed `.pack`/`.rev` object-count reductions, and a mounted honey follow-up with `find -maxdepth 8`, exact `.clang-format` hydration, and all 85 mounted symlink target checks. `home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z` closes the mounted S3 `NoSuchKey` warning row by dropping 274 warnings to 0 after exact `.tc` filenames stopped being stripped as stub aliases. `home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z` reuses the same prefix and adds mounted write/readback, cache clear/rehydrate, dirty recursive safe-unsync refusal, and clean recursive safe-unsync success. It is storage evidence with scoped project-tree parity, not production storage posture. |
| P6 | Release-binary `linux-xr` storage rerun | Functional mounted symlink closure is green in `20260511T040325Z`, and the release-binary storage rerun now has same-prefix honey mounted traversal/symlink verification, mounted warning closure, and lifecycle parity for `.pack`/`.rev` posture. The next large-tree storage packet should add a production-like TLS endpoint, generated-large-file policy, and socket accounting closure while keeping correctness success separate from production storage posture. |
| M8-A | Delete/rename while peer-unsynced tombstone details | Helper support now records stale `.tc` `sync-status`, repeated old-path pull failure, and repeated new-path hydrate success. Lower-level regressions now cover mounted old-path behavior, mounted cache eviction/rehydrate after `unsync_path`, negative-cache invalidation for peer-created paths, and delete-then-recreate same relative path. Remaining rows are host packet coverage and rename same-hash versus different-hash cases. Keep product tombstone semantics open until accepted. |
| M5-D2 | Daemon-backed conflict resolve closure | Assert the RPC returns, winning bytes remain at the original path, losing bytes land at the daemon conflict-copy path, final state is synced, a second resolve is idempotent, and timeout paths leave no partial files. |
| K1 | Keep-synced / pin acceptance | Define the product surface before testing: status wording, local storage guarantee, eviction refusal or allowance, watcher/reconcile behavior, peer delete/edit behavior, and conflict behavior. |
| R1 | Mounted remount and stale-index behavior | Mutate the remote while a local physical root is stub-only, restart/remount the daemon, and prove clean-name reads pick up the latest index without stale negative-cache behavior. Lower-level negative-cache invalidation coverage exists for peer-created paths, but this is still Linux mounted VFS evidence, not neo/macOS M4 closure. |
| F8 | Finder PZM peer-update/evict smoke | Under the PZM/testing-mode label only, mirror M3/M6 with FileProvider evict/dehydrate, peer update, requestDownload, exact latest bytes, and CLI status/log evidence. Production Finder remains blocked by Developer ID package proof. |
| M7-A | Three-machine/offline descendant matrix | Add a third device with one offline subtree, one conflicted child, one independent delete/rename sibling, and exact proof that unrelated descendants continue syncing while local conflicted content is preserved. |
