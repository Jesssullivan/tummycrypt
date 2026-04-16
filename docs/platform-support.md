# Platform Support

tcfs targets Linux, macOS, Windows, and iOS with varying feature completeness.
The repo is under active development, so "supported" here means "currently
shipped and evidenced," not "long-term stable."

## Linux (Primary)

Best-supported runtime. This is the platform with the strongest continuous CI
coverage and the clearest end-to-end validation story.

- **CLI**: All commands (push, pull, reconcile, policy, resolve, mount, unsync)
- **Daemon**: systemd user service with auto-restart, metrics (Prometheus), health checks
- **Filesystem**: FUSE3 mount with on-demand hydration, `.tc` stub files
- **NFS loopback**: Alternative to FUSE (no kernel modules required)
- **Fleet sync**: NATS JetStream with vector clock conflict detection
- **D-Bus**: Status change signals for desktop integration
- **Encryption**: XChaCha20-Poly1305 per-chunk, Argon2id KDF
- **Build targets**: x86_64 (.tar.gz, .deb, .rpm), aarch64 (.tar.gz, .deb)

## macOS (Experimental Desktop Surface)

macOS has real release artifacts and desktop integration code, but the current
evidence is weaker than Linux. Treat it as an experimental surface rather than
as a production-proven platform.

- **CLI**: Builds and ships for Apple Silicon and Intel
- **Daemon**: Launchd-oriented runtime exists, but user-facing acceptance coverage is still limited
- **FileProvider**: Packaged macOS FileProvider app exists in releases, but Finder-level claims should be treated as experimental until stronger acceptance coverage exists
- **Finder badges / progress**: Implemented in code, not yet continuously proven by system-level tests
- **Filesystem surface**: Experimental; Linux remains the better-proven mount/runtime path
- **Fleet sync**: Core sync engine and NATS path are shared with Linux, but macOS-specific acceptance coverage is not yet at the same bar
- **Encryption**: Core crypto path is shared and available
- **Build targets**: aarch64 (.tar.gz, .pkg; notarization attempted but non-blocking), x86_64 (.tar.gz)
- **Homebrew**: manual tap flow required today because the formula is published on the `homebrew-tap` branch, not the default branch
- **Current proof**: CI covers Rust builds plus Swift type-check; release workflow cuts `.pkg` and FileProvider artifacts; broader desktop UX proof is still pending
- **Current posture**: see [Apple Surface Status](ops/apple-surface-status.md)
  and [Distribution Smoke Matrix](ops/distribution-smoke-matrix.md)

### Not Yet Proven

- Fresh-install Finder/FileProvider acceptance from install through register,
  enumerate, hydrate, mutate, and conflict handling
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

- **FileProvider**: NSFileProviderExtension with enumeration + hydration
- **UniFFI**: Swift bindings via uniffi-bindgen
- **Encryption**: Full E2E decryption support
- **Build**: Swift sources type-check in CI; Xcode/TestFlight remains a manual lane
- **Status**: Proof-of-concept, not in App Store
- **Current posture**: see [Apple Surface Status](ops/apple-surface-status.md)

### Limitations
- Read-only (no upload/push from iOS)
- No background sync
- No conflict resolution UI
- Requires manual provisioning profile setup
- No continuously exercised TestFlight or App Store delivery lane

## Container (K8s Worker)

Stateless worker for horizontal scaling.

- **Image**: `ghcr.io/jesssullivan/tcfsd:latest` (distroless/cc-debian12)
- **Mode**: `--mode=worker` (NATS consumer, no FUSE)
- **Features**: k8s-worker feature flag, KEDA auto-scaling support
- **Metrics**: Prometheus on port 9100
