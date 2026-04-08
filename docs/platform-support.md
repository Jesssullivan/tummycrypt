# Platform Support

tcfs targets Linux, macOS, Windows, and iOS with varying feature completeness.

## Linux (Primary)

Full feature support. Production-ready.

- **CLI**: All commands (push, pull, reconcile, policy, resolve, mount, unsync)
- **Daemon**: systemd user service with auto-restart, metrics (Prometheus), health checks
- **Filesystem**: FUSE3 mount with on-demand hydration, `.tc` stub files
- **NFS loopback**: Alternative to FUSE (no kernel modules required)
- **Fleet sync**: NATS JetStream with vector clock conflict detection
- **D-Bus**: Status change signals for desktop integration
- **Encryption**: XChaCha20-Poly1305 per-chunk, Argon2id KDF
- **Build targets**: x86_64 (.tar.gz, .deb, .rpm), aarch64 (.tar.gz, .deb)

## macOS (Full)

Full feature support. Production-ready.

- **CLI**: All commands
- **Daemon**: launchd agent with KeepAlive, auto-restart, Developer ID signed
- **FileProvider**: Native macOS Finder integration (13,000+ items, on-demand hydration)
- **Finder Sync**: Badge overlays (synced, syncing, locked, conflict, excluded, pinned)
- **Progress**: Download progress bars in Finder via NSProgress
- **NFS loopback**: Primary mount method (no macFUSE/fuse-t dependency)
- **Fleet sync**: Full NATS JetStream support
- **Notifications**: macOS User Notifications for conflict detection
- **Encryption**: Full E2E encryption with BIP-39 recovery
- **Build targets**: aarch64 (.tar.gz, .pkg with notarization), x86_64 (.tar.gz)
- **Homebrew**: `brew install tinyland-inc/tap/tcfs`

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

## iOS (Read-only)

FileProvider extension with read-only access.

- **FileProvider**: NSFileProviderExtension with enumeration + hydration
- **UniFFI**: Swift bindings via uniffi-bindgen
- **Encryption**: Full E2E decryption support
- **Build**: Xcode project via xcodegen, type-checked in CI
- **Status**: Proof-of-concept, not in App Store

### Limitations
- Read-only (no upload/push from iOS)
- No background sync
- No conflict resolution UI
- Requires manual provisioning profile setup

## Container (K8s Worker)

Stateless worker for horizontal scaling.

- **Image**: `ghcr.io/jesssullivan/tcfsd:latest` (distroless/cc-debian12)
- **Mode**: `--mode=worker` (NATS consumer, no FUSE)
- **Features**: k8s-worker feature flag, KEDA auto-scaling support
- **Metrics**: Prometheus on port 9100
