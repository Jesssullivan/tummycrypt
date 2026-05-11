# Platform Support

tcfs targets Linux, macOS, Windows, and iOS with varying feature completeness.
This page records platform maturity; only surfaces explicitly marked proven or
available with evidence should be treated as shipped/supportable.

## Linux (Primary)

Best-supported runtime. This is the platform with the strongest continuous CI
coverage and the clearest end-to-end validation story.

- **CLI**: All commands (push, pull, reconcile, policy, resolve, mount, unsync)
- **Daemon**: package artifacts install daemon binaries and service files; isolated daemon smoke is proven, while systemd-managed service behavior is a separate release gate
- **Filesystem**: FUSE3 mount with clean-name on-demand hydration is host-proven on Linux x86_64 from repo-pinned tooling; packaged install-to-mount proof is still separate
- **Project-tree canary**: `task lazy:home-canary-linux-xr-shadow` can stage a
  real repo shadow for cross-host proof. POSIX symlink preservation is available
  through `sync_symlinks = true`, and the latest host packet proved local
  source/shadow target manifests plus symlink uploads. Full `linux-xr` parity is
  still unclaimed because mounted symlink verification and the lifecycle
  companion failed; the next packet needs rebuilt neo+honey binaries and
  matching mounted `readlink` targets
- **NFS loopback**: Alternative to FUSE (no kernel modules required), with current release evidence pending
- **Fleet sync**: NATS JetStream with vector clock conflict detection
- **D-Bus**: Interface crate exists, but the default backend is a stub and release UX/status integration is not yet proven
- **Encryption**: XChaCha20-Poly1305 per-chunk, Argon2id KDF
- **Build targets**: x86_64 (.tar.gz, .deb, .rpm) with the primary FUSE lane;
  aarch64 (.tar.gz, .deb) is install-smoke proven but cross-compiled without
  FUSE in the current release matrix. `.deb` install support is claimed for
  Ubuntu 24.04+ and Debian 13 `trixie`+. Debian 12 `bookworm` is not a truthful
  target for the current shipped `.deb` assets because the packages require
  newer glibc/OpenSSL ABI floors than bookworm provides.

## macOS (Experimental Desktop Surface)

macOS has real release artifacts and desktop integration code, but the current
evidence is weaker than Linux. Treat it as an experimental surface rather than
as a production-proven platform.

- **CLI**: Builds and ships for Apple Silicon and Intel
- **Daemon**: Launchd-oriented runtime exists, but user-facing acceptance coverage is still limited
- **FileProvider**: Packaged macOS FileProvider app exists in releases; the PZM testing-mode lab proves enumerate, hydrate, evict, rehydrate, mutation upload/readback, and CLI conflict/status content preservation, but production Finder claims remain experimental
- **Finder badges / progress**: Implemented in code and observed only as evidence; not yet a reliable release gate
- **Filesystem surface**: Experimental; Linux remains the better-proven mount/runtime path
- **Fleet sync**: Core sync engine and NATS path are shared with Linux, but macOS-specific acceptance coverage is not yet at the same bar
- **Encryption**: Core crypto path is shared and available
- **Build targets**: aarch64 (.tar.gz, .pkg; notarization attempted but non-blocking), x86_64 (.tar.gz)
- **Homebrew**: manual tap flow required today because the formula is published on the `homebrew-tap` branch, not the default branch
- **Current proof**: CI covers the Rust FileProvider staticlib/header and iOS
  Swift type-check; release workflow cuts `.pkg` and FileProvider artifacts;
  PZM testing-mode lab proof is green beyond read/hydrate; production
  clean-host Finder proof is still pending
- **Current posture**: see [Apple Surface Status](ops/apple-surface-status.md)
  and [Distribution Smoke Matrix](ops/distribution-smoke-matrix.md)

### Not Yet Proven

- Production Developer ID clean-host Finder/FileProvider acceptance from
  install through user enablement, register, enumerate, hydrate, mutate, and
  conflict handling
- Finder badges, progress UI, or notification behavior as release gates
- Every published macOS artifact on day zero without explicit post-cut smoke

## Windows (Planned)

Skeleton implementation. Not yet functional.

The `tcfs-cloudfilter` crate provides a Cloud Files API (CFAPI) skeleton for
Windows 10 1809+ placeholder files. 10 TODOs remain before functional:

### Cloud Files API Roadmap

**Provider registration** (v1.0 critical):
- `CfRegisterSyncRoot` — register sync root with shell
- `CfUnregisterSyncRoot` — cleanup on uninstall
- `CfDisconnectSyncRoot` — graceful disconnect
- `CfGetSyncRootInfoByPath` — query sync root state

**Placeholder management** (v1.0 critical):
- `CfCreatePlaceholders` — create placeholder files in Explorer
- `CfConvertToPlaceholder` + `CfSetInSyncState` — mark files as synced
- `CfDehydratePlaceholder` — convert synced file back to placeholder

**Hydration** (v1.0 important):
- `CfExecute` streaming data transfer (replace byte-return with streaming)
- `CfExecute` progress reporting (`CF_OPERATION_TYPE_REPORT_PROGRESS`)
- Cancel in-progress chunk downloads

### Blockers
- CLI uses Unix domain sockets for daemon IPC — needs TCP fallback for Windows
- No CI build matrix for Windows (disabled in release.yml)
- No Windows test infrastructure

## iOS (Proof-of-Concept)

The iOS direction exists, but it is not yet a continuously proven distribution
surface.

- **FileProvider**: NSFileProviderExtension with enumeration + hydration; experimental write hooks exist but are not accepted as supported behavior
- **UniFFI**: Swift bindings via uniffi-bindgen
- **Encryption**: Core crypto/decryption bindings are available; real-device
  credential and Files.app behavior are not yet proven
- **Build**: Swift sources type-check in CI; Xcode/TestFlight remains a manual lane
- **Status**: Proof-of-concept, not in App Store
- **Current posture**: see [Apple Surface Status](ops/apple-surface-status.md)

### Limitations
- Public posture remains read-only; write affordances may appear because hooks
  exist in code, but upload/push/delete flows are not accepted product behavior
- No background sync
- No conflict resolution UI
- Requires manual provisioning profile setup
- Shared Keychain/App Group behavior still needs real-device entitlement proof
- No continuously exercised TestFlight or App Store delivery lane

## Container (K8s Worker)

Stateless worker for horizontal scaling.

- **Current proof image**: `ghcr.io/jesssullivan/tcfsd:v0.12.12`
  (distroless/cc-debian12; `latest` is convenience-only)
- **Mode**: `--mode=worker` (NATS consumer, no FUSE)
- **Features**: k8s-worker feature flag, KEDA auto-scaling support
- **Metrics**: Prometheus on port 9100
- **Current proof**: `v0.12.12` proves explicit amd64 pull/version/startup
  only; native arm64 manifests and full Kubernetes rollout proof remain open
