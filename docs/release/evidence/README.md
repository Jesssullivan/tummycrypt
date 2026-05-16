# Release Evidence Index

This directory stores repo-archived evidence packets and links to external CI
run logs when the evidence lives in GitHub Actions artifacts instead of files in
the repository.

## Current `v0.12.12` Packets

| Packet | Scope | Evidence |
| --- | --- | --- |
| `distribution-v01212-20260508T205913Z/` | Homebrew fresh/upgrade and Darwin Nix tagged-profile install | repo-archived README and metadata |
| `container-v01212-20260509T0145Z/` | Container image current-tag smoke | native arm64 pull fails because the image index lacks `linux/arm64/v8`; explicit amd64 pull/version pass and worker startup reaches process/metrics initialization before failing on missing local NATS |
| `container-v01212-manifest-refresh-20260514T224746Z/` | Container image current-tag registry metadata refresh | `docker manifest inspect` still shows only a Linux amd64 image manifest plus an unknown/unknown manifest; local Docker/Podman runtime pull/run was not attempted because the configured Podman socket was not running |
| `linux-packages-v01212-20260509T0231Z/` | Linux package current-tag smoke | Ubuntu 24.04 `.deb` fresh/upgrade passes on arm64 and amd64; Debian 13 `.deb` fresh install passes on arm64 and amd64; Fedora 42 x86_64 daemon-only RPM fresh/upgrade passes |
| `lazy-linux-20260508T170825Z/` | Linux FUSE lifecycle on `honey`: browse before hydration, exact `cat`, mounted write/readback, cache clear/rehydrate, dirty recursive `unsync` refusal, clean recursive `.tc` conversion, persisted `NotSynced` state | repo-archived transcript, config, mount log, remote prefix, remote pullback, unsync outputs, redacted metadata |
| `fleet-pilot-20260509T1919Z/` | Isolated `Documents`/`git` fleet-pilot packet: neo seed to disposable prefix, honey mounted traversal/hydration, live `neo-honey` backend smoke | repo-archived fixture tree, transcripts, honey commands, mount log, remote prefix, and live SeaweedFS/NATS smoke log |
| `fleet-pilot-extended-20260509T2152Z/` | Extended isolated fleet-pilot packet: neo seed to disposable prefix, honey mounted traversal/hydration, honey Linux lifecycle companion, and live `neo-honey` backend smoke | repo-archived fixture tree, transcripts, honey commands, mount log, remote prefix, mounted write/readback pullback, cache rehydrate log, recursive safe-unsync outputs, and live SeaweedFS/NATS smoke log |
| `neo-honey-unsynced-rehydrate-20260510T015644Z/` | Same-fixture cross-host M3 proof: neo pushed a file, `tcfs unsync` removed neo's local copy to a `.tc` stub, honey traversed and mutated the mounted clean-name file, then neo pulled the same path | repo-archived config, state, remote prefix, honey traversal/mutation transcript, neo unsync/pull/status transcripts, exact content fixture, and `stub_after_pull=absent` |
| `neo-honey-reverse-unsynced-rehydrate-20260510T022657Z/` | First reverse same-fixture M6 attempt: honey pulled/unsynced, neo mutated/pushed, honey pulled exact content but retained a stale `.tc` stub | blocker evidence for a stale honey Linux binary that lacked pull-side adjacent-stub cleanup; superseded by the passing rerun |
| `neo-honey-reverse-unsynced-rehydrate-20260510T022858Z/` | Reverse same-fixture M6 proof: honey pulled and `tcfs unsync` removed its local copy to a `.tc` stub, neo mutated and pushed the same path, then honey pulled exact neo bytes | repo-archived config, state, remote prefix, honey pull/unsync/rehydrate/status transcripts, exact content fixtures, and `stub_after_pull=absent` |
| `neo-mounted-reverse-read-20260510T035826Z/` | M4 mounted reverse-read blocker packet: honey initial/mutated push passed, neo physical pull/unsync/status passed, then neo NFS loopback mount failed before mounted `cat` | repo-archived config, state, remote prefix, honey push transcripts, neo physical transcripts, and `neo-mount.log` showing `mount_nfs` `Operation not permitted`; no mounted read proof |
| `honey-mounted-reverse-read-20260510T042203Z/` | Linux-mounted reverse-read proof: honey pulled and unsynced a physical copy to a `.tc` stub, neo mutated and pushed the same path, then honey read exact neo bytes through a mounted clean-name view | repo-archived config, state, remote prefix, honey pull/unsync/status transcripts, mounted `ls`/`find`/`cat` transcript, exact content fixtures, and `honey_physical_after_mounted_read=stub_present`; does not close neo/macOS mount blocker |
| `neo-honey-delete-rename-unsynced-20260510T040456Z/` | M8 delete/rename while peer-unsynced current-behavior proof: honey pulled and unsynced two files, neo deleted one path and renamed another, honey old-path pulls failed and renamed new path hydrated exact bytes | repo-archived config, state, remote prefix, honey pull/unsync/delete/rename transcripts, exact content fixtures, and stale old stub status; not clean stale-placeholder UX |
| `neo-honey-conflict-20260510T043741Z/` | Cross-host same-file conflict current-behavior proof: honey pulled and edited a file, neo pushed a divergent version, then honey attempted to push its local version | repo-archived config, state, device registry, remote prefix, neo push transcripts, honey conflict transcript, `sync state: conflict`, honey local-content preservation marker, and remote pullback proving neo bytes were not overwritten |
| `neo-honey-conflict-keep-both-20260510T045810Z/` | First manual keep-both task attempt; the Taskfile alias did not forward the recovery flag, so only the existing conflict detection row ran | repo-archived detection-only conflict packet with `proof=cross-host-conflict-current-behavior`; superseded by `neo-honey-conflict-keep-both-20260510T045908Z/` |
| `neo-honey-conflict-keep-both-20260510T045908Z/` | Manual keep-both recovery proof after cross-host conflict: honey preserved its losing local bytes under a sibling path, rehydrated the original path to neo's remote bytes, pushed the sibling copy, and neo pulled both paths back | repo-archived config, state, device registry, remote prefix, conflict transcript, recovery transcript, honey sync-status before/after recovery, original-path pullback hash matching neo bytes, and conflict-copy pullback hash matching honey bytes; this is manual recovery, not `tcfs resolve` UX |
| `neo-honey-conflict-sibling-20260510T051328Z/` | Independent sibling progress proof after cross-host conflict: honey had one descendant in conflict while another edited sibling descendant pushed successfully | repo-archived config, state, device registry, remote prefix, conflict transcript, sibling push transcript, honey sync-status showing original file still `conflict` and sibling `synced`, remote pullback hash matching neo bytes for the conflicted file, and remote pullback hash matching honey bytes for the sibling |
| `neo-honey-conflict-daemon-keep-both-20260510T054020Z/` | Superseded daemon keep-both attempt: Taskfile did not forward the explicit honey `tcfsd` path, so honey selected stale `tcfsd 0.12.2` from PATH | repo-archived conflict setup plus stale daemon log; retained as task-wiring/stale-binary blocker evidence only |
| `neo-honey-conflict-daemon-keep-both-20260510T054401Z/` | Superseded daemon keep-both attempt using honey `tcfsd 0.12.12` before timeout handling was added | repo-archived conflict setup plus daemon log showing the keep-both request was accepted but the CLI resolve call hung; superseded by the bounded timeout packet |
| `neo-honey-conflict-daemon-keep-both-20260510T054611Z/` | Daemon-backed `tcfs resolve --strategy keep-both` blocker packet: honey used isolated `tcfsd 0.12.12` with auth bypass, the daemon accepted the request, but the CLI RPC timed out after 30s | repo-archived config, state, device registry, remote prefix, conflict transcript, daemon log, timeout result, post-timeout pullbacks proving original remote bytes remained neo's and daemon-created conflict-copy bytes matched honey's; clean daemon resolve completion is not claimed |
| `home-canary-linux-xr-shadow-20260510T002604Z/` | Local real project-tree shadow of `/Users/jess/git/linux-xr`: read-only source inventory, full isolated 7.9 GB shadow under `~/TCFS Pilot/real-canaries/`, disposable raw `.git`/hidden-dir config, and honey command packet | repo-archived inventory/config metadata; push/honey/lifecycle were not run; full project parity is explicitly blocked because this archived packet did not prove the 85 inventoried symlinks as preserved links |
| `home-canary-linux-xr-shadow-20260510T023938Z/` | Real project-tree shadow canary against `/Users/jess/git/linux-xr`: completed 7.7 GB shadow push, honey mounted clean-name traversal/hydration, and Linux lifecycle companion | repo-archived source/shadow inventory, raw `.git`/hidden-dir config, completed `push.log`, honey mounted `ls`/`find -maxdepth 3`/`cat` transcript for `.clang-format`, mount log, Linux lifecycle write/readback/cache-clear/rehydrate/safe-unsync transcripts, and `result.env` with `proof=shadow-push-honey-linux-lifecycle`; full project parity remains blocked because this archived packet skipped 85 symlinks |
| `home-canary-linux-xr-shadow-20260510T201809Z/` | Partial/blocking real project-tree shadow canary with `sync_symlinks = true`: completed 7.7 GB shadow push and local source/shadow target manifests matched for 85 symlinks | repo-archived source/shadow inventory, symlink target manifests, completed `push.log` with 93,054 uploads and 85 symlink uploads, honey command/mount logs, failed honey mounted symlink verification at `Documentation/Changes`, failed Linux lifecycle mounted `cat`, and pre-fix S3 posture observations; not full project parity |
| `home-canary-linux-xr-shadow-20260511T040325Z/` | Scoped `linux-xr` isolated-shadow project-tree parity canary with `sync_symlinks = true`: reused the completed 7.7 GB push, honey mounted bounded traversal/hydration passed, 85 symlink targets matched through mounted `readlink`, and the Linux lifecycle companion passed | repo-archived source/shadow inventory, symlink target manifests, completed `push.log`, push storage summary, honey mounted `ls`/`find -maxdepth 8`/`cat` transcript, mounted symlink verification, mount log, Linux lifecycle write/readback/cache-clear/rehydrate/safe-unsync transcripts, and `result.env` with `proof=shadow-push-honey-linux-lifecycle-symlink-targets`; this is functional isolated project-tree evidence, not production Finder, broad home-directory, or production S3 posture proof |
| `home-canary-linux-xr-storage-posture-20260512T034347Z/` | Partial release-binary/fresh-prefix `linux-xr` storage-posture blocker packet: the 6.2 GB raw-Git pack completed, the adjacent `.rev` completed, and the run then exposed slow small-file upload behavior before being stopped | repo-archived source/shadow inventory, symlink target manifests, partial `push.log`, storage summary with 4,046 upload rows / 6.69 GB / 91,724 chunks / no retry rows, live process/socket samples, release binary provenance, and `storage-posture-live-observations.md`; `result.env` records `proof=push-failed`, so this is not scoped project-tree parity or production S3 posture evidence |
| `home-canary-linux-xr-storage-posture-20260512T125747Z/` | Partial release-binary storage-posture context packet after the timeout work but before the final file-concurrency merge: source/shadow inventories and symlink manifests were captured, and the long push log reached the small-file walk before the packet remained pending | repo-archived inventories, symlink target manifests, redacted endpoint/credential-presence metadata, partial `push.log`, and `storage-posture-live-observations.md`; no honey traversal, mounted symlink verification, Linux lifecycle, or production storage posture is claimed |
| `home-canary-linux-xr-storage-posture-20260513T174944Z/` | Post-PR #367 release-binary/fresh-prefix file-concurrency rerun from `main` `9428513`: bounded file/chunk upload concurrency was active and timeout/transport retry telemetry fired, but the 6.2 GB raw-Git pack reached only 853 / 70,856 chunks before the run was intentionally stopped | repo-archived source/shadow inventory, symlink target manifests, `file_upload_concurrency=8`, `chunk_upload_concurrency=8`, timeout/retry rows, socket samples, partial `push.log`, and `storage-posture-live-observations.md`; `result.env` records push failure, so this is a storage blocker packet, not scoped project-tree parity or production S3 posture evidence |
| `home-canary-linux-xr-storage-posture-20260513T220442Z/` | Push-only release-binary/fresh-prefix storage-posture rerun from `main` `74ac016`: completed the 7.7 GB `linux-xr` shadow push and proved the raw Git `.pack` large sequential profile reduced the dominant 6.2 GB pack from 70,856 chunks to 1,211 chunks | repo-archived source/shadow inventory, symlink target manifests, release binary SHA-256, compressed full `push.log.gz`, `push-storage-summary.env`, socket samples showing highwater 11 vs concurrency 8, and `storage-posture-live-observations.md`; honey traversal/lifecycle were disabled, the endpoint was plaintext HTTP, and `.rev` still leaked 8,405 chunks before the follow-up profile fix, so this is not production S3 posture or full parity evidence |
| `home-canary-linux-xr-storage-posture-20260514T021513Z/` | Release-binary/fresh-prefix storage-posture rerun from `main` `c0c2c0c`: completed the 7.7 GB `linux-xr` shadow push, proved the raw Git `.rev` large sequential profile reduced the dominant 45.6 MB reverse index from 8,405 chunks to 8 chunks, and added same-prefix honey mounted traversal/symlink verification | repo-archived source/shadow inventory, symlink target manifests, release binary SHA-256, compressed full `push.log.gz`, `push-storage-summary.env` with 92,969 upload rows / 8.23 GB / 327,482 chunks / no retry or error rows, socket samples showing highwater 11 vs concurrency 8, `mounted-followup.env`, honey mounted `find -maxdepth 8`/exact `.clang-format` hydration/all 85 mounted symlink target checks, and `storage-posture-live-observations.md`; Linux lifecycle was not run in this storage packet, the endpoint was plaintext HTTP, `.idx` still reached 4,599 chunks, and generated AMD header files reached 2,986/2,121 chunks, so this is not production S3 posture or full parity evidence |
| `home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/` | Same-prefix lifecycle companion for the `20260514T021513Z` storage packet: reused the completed shadow/prefix, reran honey mounted traversal/symlink verification with the exact `.tc` filename fix binary, and added the Linux lifecycle companion | repo-archived source/shadow inventory refresh, relative link to the original compressed push log, regenerated storage summary, honey mounted `find -maxdepth 8`/exact `.clang-format` hydration/all 85 mounted symlink target checks with 0 actual honey `WARN`/`ERROR`/`NoSuchKey` rows, Linux lifecycle mounted write/readback/cache-clear/rehydrate/recursive safe-unsync transcripts, and `parity-gates.env` with `status=scoped-project-tree-parity-evidence-complete`; endpoint/TLS, socket highwater, `.idx`, generated-large-file policy, production Finder, and broad home takeover remain out of scope |
| `git-repo-canary-oauth-mux-20260515T000411Z/` | Plan-only shadow-first generic git repo canary against clean `~/git/oauth-mux` on branch `codex/clarify-claude-adapter-reality` | repo-archived policy, source/shadow inventory, dirty-status proof, symlink target comparison, selected hydration fixture, generated TCFS config, and inherited honey command packet; no push, honey traversal, Linux lifecycle, live repo mutation, production Finder, broad `~/git`, or home-directory takeover is claimed |
| `git-repo-canary-oauth-mux-20260515T003124Z/` | Stopped first live generic git repo canary against clean `~/git/oauth-mux`; records the release-binary symlink push blocker and slow sequential push shape | release-binary push transcript starts with nine `skipping symlink (follow_symlinks=false)` rows, so `parity-gates.env` keeps `status=full-project-parity-not-claimed`; run was stopped after the blocker was observed and does not claim push completion, honey traversal, Linux lifecycle, Finder readiness, broad `~/git`, or home-directory takeover |
| `git-repo-canary-oauth-mux-20260515T003543Z/` | Stopped live generic git repo canary against clean `~/git/oauth-mux`; records a symlink push blocker | release-binary push transcript starts with nine `skipping symlink (follow_symlinks=false)` rows, so `parity-gates.env` keeps `status=full-project-parity-not-claimed`; run was stopped after the blocker was observed and does not claim push completion, honey traversal, Linux lifecycle, Finder readiness, broad `~/git`, or home-directory takeover |
| `tcfs-symlink-config-probe-20260515T005858Z/` | Tiny disposable symlink config probe comparing installed Homebrew `tcfs 0.12.12` to source-built `target/codex-verify/debug/tcfs` | same `sync_symlinks = true` fixture: Homebrew skipped `link.txt -> target.txt`, while source-built `main` preserved the symlink and uploaded two entries; this narrows the generic repo canary blocker to packaged-binary divergence and does not claim production readiness, Finder readiness, broad home takeover, or completed repo parity |
| `tcfs-symlink-package-probe-20260515T041947Z/` | Repeatable package/current symlink probe comparing installed Homebrew, source-built local, and current-checkout Nix `tcfs 0.12.12` binaries | same `sync_symlinks = true` fixture: Homebrew `b93824d...` skipped `link.txt -> target.txt`; source-built `b2a970...` and Nix current `2ca9e1...` preserved the symlink and uploaded two entries; `overall_status=blocked` because the published Homebrew package remains stale, so this narrows the next dogfood gate to Homebrew rebuild/publish plus cross-host mounted parse proof |
| `tcfs-symlink-package-probe-20260515T051126Z/` | Tiny mounted parse/target proof for neo current-checkout Nix producer to honey current source-built Linux consumer | Nix current `2ca9e1...` preserved `link.txt -> target.txt`, honey mounted the disposable prefix with `/tmp/tcfs-current-srcbin-a76d48db3e06/bin/tcfs` `dc9b17...`, listed clean names, verified the symlink target, and `cat` hydrated exact `target.txt`; `overall_status=passed`, but this is not Homebrew/current packaged-consumer proof because honey's packaged profile remains `tcfs 0.12.2` |
| `tcfs-symlink-package-probe-20260515T060330Z/` | Tiny mounted parse/target proof for current Nix flake package producer and current Nix flake package consumer | neo `nix build .#tcfs-cli` produced `/nix/store/gp0...-tcfs-cli-0.12.12` `a58ef5...`, honey `nix build github:Jesssullivan/tummycrypt/b4a1ba7#tcfs-cli` produced `/nix/store/yw9...-tcfs-cli-0.12.12` `dc9b17...`, Nix preserved `link.txt -> target.txt`, and honey mounted/listed/read the disposable prefix with `overall_status=passed`; this narrows the package blocker to Homebrew/current-tag publication and package-backed repo canary proof |
| `git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/` | Green source-built generic git repo canary against clean `~/git/oauth-mux`: source-built neo push, current-source honey mounted traversal/hydration, mounted symlink target verification, and Linux lifecycle companion | repo-archived source/shadow inventory, completed fresh-prefix push with 4,593 uploaded file rows / 356,107,080 bytes / zero skipped symlinks / all rows `fresh_prefix_publish=true`, push-time metadata, honey smoke with explicit source-built Linux binary SHA, mounted symlink `readlink` checks for 9 symlinks, Linux lifecycle write/readback/cache-clear/rehydrate/recursive safe-unsync transcripts, and `result.env` with `proof=shadow-push-honey-linux-lifecycle-symlink-targets`; this is isolated shadow proof only, not live repo takeover or packaged-binary readiness |
| `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/` | Green current Nix flake package-backed generic git repo canary against clean `~/git/oauth-mux`: Nix package neo push, Nix package honey mounted traversal/hydration, mounted symlink target verification, and Linux lifecycle companion; restore blocker now archived under `restore-proof/` | repo-archived source/shadow inventory, explicit Nix build/version/SHA files for both binaries, completed fresh-prefix push with 4,601 uploaded file rows / 356,520,343 bytes / zero skipped symlinks / all rows `fresh_prefix_publish=true`, honey smoke with Nix package SHA `dc9b17...`, mounted symlink `readlink` checks for 9 symlinks, Linux lifecycle write/readback/cache-clear/rehydrate/recursive safe-unsync transcripts, and `result.env` with `proof=shadow-push-honey-linux-lifecycle-symlink-targets`; `restore-proof/restore-proof.env` records `proof=fresh-tree-restore-blocked` because `tcfs reconcile` dry-run timed out after 120s while scanning the remote index, before restore execution; this closes the current Nix package-backed small-repo shadow gate only, not restore/rollback, Homebrew readiness, live repo takeover, production Finder, broad home takeover, or production storage posture |
| `git-repo-canary-linux-xr-fast-sourcefix-20260516T024122Z/` | Source-built `linux-xr-fast` blocker packet after the pack-index chunk-profile fix | live source remained read-only; source/shadow inventories record clean `xr/main` at `dbfcd3938a2f38cd1020716e98aad245452f51e1`, 2,038 regular files, 0 symlinks, and 0 unsupported special files. Push evidence showed the large pack indexes reduced to 75 and 51 chunks, then exposed extensionless `.git/objects/pack/tmp_pack_*` files at 52,372 / 17,057 / 6,395 chunks. No honey/lifecycle/restore proof is claimed |
| `git-repo-canary-linux-xr-fast-sourcefix-tmppack-20260516T024810Z/` | Source-built `linux-xr-fast` blocker packet after the temp-pack chunk-profile fix | reused the same shadow and fresh prefix, then was intentionally stopped with `status=143`; temp packs reduced to 51 / 18 / 8 chunks, large pack indexes stayed at 75 and 51 chunks, and the 10 MB `.git/index` became the max-chunk path at 1,767 chunks before the current `.git/index` source fix. No green push, honey traversal, Linux lifecycle, restore, live repo move, Finder, broad home, or production storage claim is made |
| `git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/` | Source-built `linux-xr-fast` green push/mount/lifecycle packet with fresh-tree restore blocker | completed isolated shadow push from `~/git/linux-xr-fast` with 2,038 uploaded regular-file rows / 8,702,124,366 file bytes / 6,740 chunks / zero skipped symlinks, proving Git pack-index, temp-pack, and exact `.git/index` profile fixes in one run; honey mounted traversal/hydration and Linux lifecycle companion passed. `restore-proof/restore-proof.env` is failed: 2,036 of 2,038 regular files restored, all 6 empty dirs matched, and two multi-GB raw Git `.pack` files failed after transient chunk read errors. No live repo move, package-backed restore/rollback, Homebrew, Finder, broad `~/git`, or home takeover claim is made |
| `macos-fileprovider-neo-preflight-20260516T023852Z/` | Refreshed neo FileProvider divergence inventory and strict preflight blocker | repo-archived PATH/version/app-location/PlugInKit/CloudStorage/config/socket/launchd/bounded-`~/tcfs` inventory; PlugInKit pointed at `~/Applications/TCFSProvider.app`, strict preflight failed on missing host/extension Keychain access-group entitlements and embedded provisioning profiles, ambient `tcfs` was workspace `0.12.12`, and ambient `tcfsd` was Nix profile `0.12.2`; no production Finder claim |
| `macos-fileprovider-neo-pkg-install-20260516T024006Z/` | Helper-based published `.pkg` install attempt after refreshed divergence inventory | package checksum/signature/notarization were verified, stale `~/Applications/TCFSProvider.app` was quarantined under the evidence packet, and install remained blocked because `sudo -n installer` required a password; `/Applications/TCFSProvider.app` was not installed, so strict preflight/Finder lifecycle remain open |
| `macos-fileprovider-neo-cleanup-20260516T073644Z/` | Refreshed canonical neo FileProvider preflight packet | inventory confirms `/Applications/TCFSProvider.app` is absent, the only visible app remains `~/Applications/TCFSProvider.app`, PlugInKit is parented by that user app, ambient `tcfs` is workspace `0.12.12`, and ambient `tcfsd` is Nix profile `0.12.2`; strict preflight fails before signing checks because the canonical app path is missing |
| `macos-fileprovider-userapp-preflight-20260516T073758Z/` | Strict preflight against the registered neo user app | codesign validation and App Group entitlements are present, but production preflight fails because host and extension lack Keychain access-group entitlements and embedded provisioning profiles; local profile inventory separately found a compatible Developer ID profile pair that still needs to be embedded into a package-installed app |
| `macos-fileprovider-signed-app-preflight-20260516T183213Z/` | Source-built Developer ID signed app preflight on neo | source-built Rust FileProvider bridge and Swift app assembly passed with embedded compatible local Developer ID host/extension profiles; direct strict signing-only preflight passed for host and extension App Group plus keychain-access-groups entitlements and embedded profiles. This is non-installing source-built signing proof only, not `.pkg` install, PlugInKit cleanup, Finder lifecycle, or production clean-host readiness |
| `macos-fileprovider-candidate-pkg-20260516T190702Z/` | Non-installing candidate `.pkg` structure/signature proof on neo | current source-built `tcfs`/`tcfsd 0.12.12` plus the signed source-built FileProvider app were wrapped into a local `.pkg`; payload structure smoke passed for `/usr/local/bin/tcfs`, `/usr/local/bin/tcfsd`, `/Applications/TCFSProvider.app`, the FileProvider appex, and repo postinstall; package signature verifies as Developer ID Installer with trusted timestamp. This is still not install, PlugInKit cleanup, or Finder lifecycle proof |
| `macos-fileprovider-candidate-pkg-assessment-20260516T194612Z/` | Non-installing Gatekeeper/notarization assessment of the candidate `.pkg` | `pkgutil --check-signature` still passes and the expanded payload matches the expected app/appex/CLI/postinstall shape, but `spctl --assess --type install` rejects the package as `Unnotarized Developer ID` and `xcrun stapler validate` reports no stapled ticket. The next package gate is notarize/staple before install/preflight/Finder smoke |
| `macos-fileprovider-neo-cleanup-20260510T003148Z/` | Non-mutating neo FileProvider divergence inventory before cleanup | repo-archived PATH/version/app-location/PlugInKit/CloudStorage/config/socket/launchd/bounded-`~/tcfs` inventory; no `.pkg` install, stale app quarantine, or strict production preflight was run |
| `macos-fileprovider-neo-cleanup-pkg-20260510T0036Z/` | Non-mutating neo FileProvider divergence inventory with the published `v0.12.12` `.pkg` selected as source | repo-archived inventory plus package checksum pass, `pkgutil --check-signature` Developer ID/notarization output, and non-installing package structure smoke; no `.pkg` install, stale app quarantine, or strict production preflight was run |
| `macos-fileprovider-strict-preflight-blocker-20260510T0040Z/` | Non-mutating strict production signing preflight against the existing `~/Applications/TCFSProvider.app` | preflight failed as expected: host and extension keychain access group entitlements/provisioning profiles are missing; ambient `tcfsd` is still `0.12.2` from the Nix profile; this is a blocker record, not Finder readiness |
| `macos-pkg-install-attempt-20260510T0045Z/` | Non-interactive install attempt for the published `v0.12.12` macOS `.pkg` | blocked: `sudo -n installer` exited with status 1 because a password is required; `/Applications/TCFSProvider.app` remained absent; no stale user app quarantine occurred |
| `macos-fileprovider-neo-cleanup-install-blocker-20260510T0048Z/` | Helper-based published `.pkg` install attempt after divergence inventory | blocked: `sudo -n installer` exited with status 1 because a password is required; the helper still wrote inventory, README, and install status; `/Applications/TCFSProvider.app` remained absent |
| Production Developer ID `.pkg` hosted smoke attempt | Published `v0.12.12` package on GitHub-hosted `macos-15`: package download, structure check, install, installed FileProvider signing, installed CLI smoke, live config, and FileProvider config provisioning passed; remote fixture seed failed because the public Cloudflare quick-tunnel hostname did not resolve from the hosted runner | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25613963424> |
| PZM testing-mode FileProvider package run | Mac App Development/testing-mode package build for deterministic conflict/status proof | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25569345240> |
| PZM testing-mode FileProvider smoke run | Enumerate/hydrate/evict/rehydrate, mutation proof already present from prior run, deterministic CLI conflict/status and exact FileProvider content preservation | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25569596910> |
| PZM testing-mode mutation package run | Mac App Development/testing-mode package build for mutation proof | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25565895586> |
| PZM testing-mode mutation smoke run | CloudStorage mutation upload and exact 68-byte remote pullback | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25565943781> |
| PZM testing-mode evict/rehydrate smoke run | Installed host policy probe, E2EE, daemon startup, FileProvider registration, enumeration, requestDownload, evict, re-requestDownload, exact 55-byte hydration | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25562087555> |

## Scope Notes

- `neo-honey-unsynced-rehydrate-20260510T015644Z/` closes the first
  same-fixture cross-host rehydrate row. It proves neo can remove a clean local
  copy with `tcfs unsync`, honey can edit that file through a mounted view, and
  neo can pull exact honey bytes with `sync state: synced` and no stale adjacent
  `.tc` file.
- `neo-honey-reverse-unsynced-rehydrate-20260510T022858Z/` closes the reverse
  same-fixture row. It proves honey can pull and unsync a physical copy, neo can
  mutate and push the same path, and honey can pull exact neo bytes with
  `sync state: synced` and no stale adjacent `.tc` file.
- `neo-honey-reverse-unsynced-rehydrate-20260510T022657Z/` intentionally stays
  indexed as blocker evidence. It showed the reverse row fails if the peer host
  still runs an older `tcfs` binary without pull-side stale-stub cleanup.
- `neo-mounted-reverse-read-20260510T035826Z/` is the current M4 live blocker.
  It proves the pre-mount stages against a disposable prefix, but not the
  mounted reverse read itself: neo/macOS has no macFUSE installed, and the NFS
  loopback fallback failed at `mount_nfs` with `Operation not permitted`.
- `honey-mounted-reverse-read-20260510T042203Z/` closes the Linux-mounted
  equivalent of the reverse-read row. Honey pulled and unsynced a physical copy,
  neo mutated and pushed the same path, and honey's mounted clean-name surface
  listed and `cat`-read exact neo bytes while the physical honey root remained
  stub-only. This is mounted VFS evidence, not production Finder evidence, and
  it does not remove the neo/macOS mount blocker above.
- `neo-honey-delete-rename-unsynced-20260510T040456Z/` proves M8 current
  behavior only. Delete and rename old-path pulls fail deterministically, and
  the renamed new path hydrates exact bytes, but old physical `.tc` stubs remain
  present on honey. The helper uses the current safe order for same-hash rename:
  delete the old remote path before publishing the new path. Clean tombstone or
  stale-placeholder cleanup UX remains open; full conflict permutations also
  remain open QA rows.
- `neo-honey-conflict-20260510T043741Z/` closes the first cross-host
  same-file conflict row at CLI/current-behavior depth. Honey pulled and edited
  the file, neo pushed divergent bytes, honey's later push reported
  `CONFLICT`, skipped upload, marked `sync state: conflict`, preserved honey's
  local bytes, and a neo pullback proved the remote still held neo's bytes. This
  does not prove Finder conflict UI, automatic conflict resolution, or
  keep-synced/pin semantics.
- `neo-honey-conflict-keep-both-20260510T045810Z/` is retained as superseded
  task-wiring evidence. It used the intended keep-both prefix, but the Taskfile
  alias had not forwarded `--honey-recover-keep-both`, so the packet only
  repeated the detection/preservation proof.
- `neo-honey-conflict-keep-both-20260510T045908Z/` extends the conflict row with
  a manual keep-both recovery pattern. After honey recorded conflict state, it
  copied the losing honey bytes to
  `Projects/shared/conflict-notes.conflict-honey.md`, pulled the original path
  back to neo's remote bytes, pushed the sibling copy, and neo pulled both paths
  back with exact hash matches. This is a scriptable recovery pattern, not
  daemon-backed `tcfs resolve`, Finder conflict UI, or automatic resolution.
- `neo-honey-conflict-sibling-20260510T051328Z/` adds a partial M7 descendant
  permutation. Honey records conflict on `Projects/shared/conflict-notes.md`,
  then pushes `Projects/shared/conflict-independent-sibling.md`; sync-status
  keeps the original file at `conflict` while the sibling reports `synced`, and
  neo pullbacks prove the conflicted file still has neo bytes while the sibling
  has honey bytes. This proves per-path sibling progress, not conflict-list UX
  or automatic resolution.
- `neo-honey-conflict-daemon-keep-both-20260510T054020Z/` and
  `neo-honey-conflict-daemon-keep-both-20260510T054401Z/` are retained as
  superseded daemon-resolution blocker packets: first a stale honey `tcfsd`
  path-selection bug, then an unbounded hang with the explicit `0.12.12` daemon.
- `neo-honey-conflict-daemon-keep-both-20260510T054611Z/` is the bounded
  daemon-resolution blocker. It proves the helper can start isolated honey
  `tcfsd 0.12.12` and reach `ResolveConflict(keep_both)` under
  `auth.require_session=false`, but the CLI RPC timed out after 30 seconds.
  Post-timeout pullbacks show partial side effects: the original remote path
  still matched neo bytes and the daemon-created conflict copy matched honey
  bytes. This is not a clean `tcfs resolve` UX claim.
- `home-canary-linux-xr-shadow-20260510T002604Z/` did not mutate
  `/Users/jess/git/linux-xr` and did not contact remote storage. It proves the
  local source inventory and shadow/config packet only. Push, honey traversal,
  Linux lifecycle, and full project parity remain open because the source
  contains 85 symlinks and this packet did not prove them as preserved links.
- `home-canary-linux-xr-shadow-20260510T023938Z/` is the earlier scoped real
  project-tree canary packet. The original long push completed with 92,969
  files and 7.7 GB uploaded, then the helper resumed from the completed
  `push.log`/state without re-pushing. Honey mounted the prefix, listed clean
  names including `.git`, ran bounded traversal at `max-depth=3`, and `cat`
  hydrated `.clang-format` with exact 24,291-byte content. The Linux lifecycle
  companion under the same prefix also passed mounted write/readback, cache
  clear/rehydrate, dirty safe-unsync refusal, clean recursive unsync, and exact
  rehydrate. It remains useful as a pre-symlink baseline; the source contains
  85 symlinks that this packet did not prove as preserved links.
- `home-canary-linux-xr-shadow-20260510T201809Z/` is the fresh symlink-enabled
  canary attempt and is intentionally indexed as blocker/storage evidence. It
  preserved all 85 source symlink targets in the isolated shadow, pushed the
  shadow with `sync_symlinks = true`, and recorded 85 symlink uploads, but honey
  mounted symlink verification failed on `Documentation/Changes` and the Linux
  lifecycle companion failed during mounted `cat`. Its S3 notes captured
  pre-fix large raw-Git behavior, including `.idx` tiny-object expansion and
  large `.pack` snapshot memory.
- `home-canary-linux-xr-shadow-20260511T040325Z/` is the current scoped
  isolated-shadow `linux-xr` project-tree parity packet. It did not mutate the
  live source repo. The push completed before the final storage telemetry
  changes, then the helper resumed from the completed `push.log`/state without
  re-pushing. Honey mounted the prefix, ran bounded traversal at `max-depth=8`,
  hydrated `.clang-format` with exact 24,291-byte content, verified all 85
  mounted symlink targets with `readlink`, and passed the Linux lifecycle
  companion. This closes the scoped project-tree parity bar for this isolated
  shadow only; production Finder, broad home-directory takeover, neo/macOS M4,
  tombstone UX, keep-synced/pin semantics, and production S3 posture remain
  separate open gates.
- `git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/` closes the
  first small clean real-repo shadow canary when source-built binaries are
  explicitly selected on both hosts. `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/`
  closes the same small-repo shadow canary for explicitly selected current Nix
  flake package binaries on both hosts. `tcfs-symlink-package-probe-20260515T041947Z/`
  narrows the package gap: current-checkout Nix and source-built binaries
  preserve symlinks, but installed Homebrew `0.12.12` still skips them.
  `tcfs-symlink-package-probe-20260515T051126Z/` proves the tiny symlink index
  mounts cleanly from neo current-checkout Nix to honey current source-built
  Linux. `tcfs-symlink-package-probe-20260515T060330Z/` proves the same tiny
  mounted parse/target check with current Nix flake packages on both hosts.
  Version strings alone are not enough for repo dogfood readiness.
  `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof/` records
  the next restore blocker: `tcfs reconcile` dry-run timed out after 120s while
  scanning the remote index. The later source-built
  `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof-source-fix-empty-dirs-20260515T183805Z/`
  packet proves fresh-tree restore for 4,601 regular files, 9 symlinks, synced
  state for 4,610 paths, and all 12 empty directories with
  `--require-empty-dirs`. Rebuild/publish Homebrew if Homebrew is the client
  lane, prove packaged restore, then rerun the larger clean `linux-xr-fast`
  stress canary with a selected package/binary and green package-backed
  restore/rollback proof before moving any live repo into TCFS. The first
  `linux-xr-fast` package attempts are blocker packets:
  `git-repo-canary-linux-xr-fast-nixpkg-20260516T005236Z/` and
  `git-repo-canary-linux-xr-fast-nixpkg-tuned-20260516T010911Z/` both stop at
  the same 387 MB `.git/objects/pack/*.idx` upload before honey/lifecycle
  proof. Follow-up source-built packets prove pack-index, temp-pack, and exact
  `.git/index` reductions. The current source-built packet,
  `git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/`, is green
  for large shadow push, honey mounted traversal/hydration, and Linux lifecycle,
  but its fresh-tree restore failed after restoring 2,036 of 2,038 regular
  files and all 6 empty dirs; the two missing files are multi-GB raw Git pack
  files. No package-backed restore/rollback proof, live repo move, broad home,
  or production storage claim exists yet.
- The runnable `macos-fileprovider-neo-cleanup-<UTC>/` packet is divergence
  inventory and optional cleanup/install evidence. It is not production Finder
  readiness unless strict production signing preflight passes against the
  published `.pkg` install.
- `macos-fileprovider-neo-cleanup-20260510T003148Z/` is inventory-only. It
  recorded ambient divergence (`tcfs` from `target/debug`, `tcfsd` from the Nix
  profile, and `~/Applications/TCFSProvider.app` present), but it did not
  install the published package or unregister/quarantine stale registrations.
- `macos-fileprovider-neo-cleanup-pkg-20260510T0036Z/` verifies the published
  package source without installing it. The package checksum and structure
  smoke passed, and `pkgutil --check-signature` reported Developer ID Installer
  signing plus Apple notarization. Production Finder remains open because the
  package was not installed and strict preflight/Finder lifecycle did not run.
- `macos-fileprovider-strict-preflight-blocker-20260510T0040Z/` is an explicit
  blocker capture for the existing user app. It failed strict production
  signing preflight because keychain access group entitlements and provisioning
  profiles were missing. This reinforces that local Finder smoke must not be
  described as production-adjacent from the stale user app.
- `macos-pkg-install-attempt-20260510T0045Z/` shows that this non-interactive
  shell cannot install the published package into `/Applications` because sudo
  requires a password. The package source remains verified, but install and
  post-install Finder preflight remain open on this host.
- `macos-fileprovider-neo-cleanup-install-blocker-20260510T0048Z/` repeats the
  same blocked install path through the cleanup helper after the helper was
  hardened to use `sudo -n` and still write complete failure evidence.
- `macos-fileprovider-neo-preflight-20260516T023852Z/` refreshes the local neo
  divergence record: PlugInKit still pointed at the user app, strict preflight
  failed on missing production Keychain/provisioning material, and ambient
  `tcfsd` was stale at `0.12.2`.
- `macos-fileprovider-neo-pkg-install-20260516T024006Z/` verifies the published
  package, quarantines the stale user app, and records that the remaining local
  install blocker is admin authentication for `sudo installer`. This is useful
  cleanup progress, not Finder readiness.
- PZM FileProvider runs are non-production Mac App Development/testing-mode
  evidence with the lab `SystemPolicyRule` profile. They do not prove a
  production Developer ID clean-host Finder lane.
- The production Developer ID `.pkg` hosted smoke attempt is useful package,
  signing, and installed-binary evidence, but it failed before daemon startup
  and FileProvider lifecycle because the run-scoped storage fixture could not
  be seeded through the expired/unresolvable public tunnel endpoint.
- `distribution-v01212-20260508T205913Z/` covers Homebrew and Nix only. It does
  not cover current-tag Linux packages, container, or production macOS `.pkg`
  smoke.
- `container-v01212-20260509T0145Z/` covers the `v0.12.12` container image
  only. It proves amd64 image presence/version/startup logs and records a
  missing native arm64 manifest.
- `linux-packages-v01212-20260509T0231Z/` covers Linux package install/upgrade
  smoke, not mounted FUSE lifecycle or production systemd service management.
- Linux `lazy-linux-20260508T170825Z/` proves the mounted lifecycle and
  recursive safe-unsync behavior, not Linux package fresh/upgrade install.
- `fleet-pilot-20260509T1919Z/` proves an isolated cross-host pilot tree and
  live backend smoke. It does not prove production Finder, mounted writeback,
  recursive safe-unsync, or managing real `~/Documents` / `~/git`.
- `fleet-pilot-extended-20260509T2152Z/` adds honey-side mounted
  write/readback, cache clear/rehydrate, and recursive safe-unsync evidence
  through a nested Linux lifecycle companion. It still does not prove
  production Finder, production Developer ID FileProvider acceptance, live
  OpenTofu/on-prem cutover, or managing real `~/Documents` / `~/git`.
