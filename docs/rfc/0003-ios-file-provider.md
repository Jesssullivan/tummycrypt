# RFC 0003: iOS File Provider Extension

**Status**: Experimental scaffold and reference design
**Author**: xoxd
**Date**: 2026-02-22 (updated 2026-04-15)
**Tracking**: Swift sources + build script exist, but the public iOS posture remains read-only proof-of-concept pending stronger acceptance coverage

---

## Abstract

This RFC describes the architecture for an iOS File Provider extension that exposes
tcfs storage in the iOS Files app. The extension reuses existing Rust crates
(tcfs-storage, tcfs-chunks, tcfs-crypto, tcfs-sync) via Mozilla UniFFI, bridging
to Swift for the native FileProviderExtension API.

Current posture note: this is still an experimental surface. The repo has
working scaffolding and build scripts, but not a continuously proven iOS
distribution lane. See [iOS Surface Status](../ops/ios-surface-status.md) and
[Apple Surface Status](../ops/apple-surface-status.md).

## Motivation

iOS remains an experimental target for tcfs. The intent is for users to
eventually be able to:

- Browse their tcfs files in the iOS Files app
- Open files on-demand (hydration from SeaweedFS)
- Share files between iOS and desktop machines via the same CAS backend
- Benefit from E2E encryption on mobile

The iOS File Provider framework provides the system integration point, similar to
how tcfs-fuse provides Linux integration and tcfs-cloudfilter provides Windows
integration.

## Current Maintenance Scope

As of April 15, 2026:

- keep the iOS Rust and Swift surfaces buildable in CI
- treat browsing, enumeration, and hydration as the documented product direction
- treat create, modify, and delete hooks as experimental code paths rather than
  supported user-facing scope
- keep TestFlight and App Store tooling manual-only until there is a repeatable
  distribution lane

## Architecture

```
┌──────────────────────────────────────────────────┐
│                    iOS Files App                  │
├──────────────────────────────────────────────────┤
│           NSFileProviderExtension                │
│  ┌────────────────────────────────────────────┐  │
│  │              Swift Layer                    │  │
│  │  - FileProviderExtension.swift             │  │
│  │  - FileProviderItem.swift                  │  │
│  │  - FileProviderEnumerator.swift            │  │
│  │  - ContentKeychain.swift (~2000 LOC)       │  │
│  └────────────────┬───────────────────────────┘  │
│                   │ UniFFI (C ABI)               │
│  ┌────────────────┴───────────────────────────┐  │
│  │          tcfs-file-provider (Rust)          │  │
│  │  - uniffi bindings (~1000 LOC)             │  │
│  │  - async task bridge                       │  │
│  │  - credential adapter (Keychain)           │  │
│  └────────────────┬───────────────────────────┘  │
│                   │                              │
│  ┌────────────────┴───────────────────────────┐  │
│  │           Reused tcfs Crates               │  │
│  │  ┌─────────────┐  ┌──────────────┐        │  │
│  │  │ tcfs-storage │  │  tcfs-chunks │        │  │
│  │  │ (70% reuse)  │  │ (100% reuse) │        │  │
│  │  └──────────────┘  └──────────────┘        │  │
│  │  ┌─────────────┐  ┌──────────────┐        │  │
│  │  │  tcfs-sync  │  │  tcfs-crypto │        │  │
│  │  │ (80% reuse) │  │ (100% reuse) │        │  │
│  │  └──────────────┘  └──────────────┘        │  │
│  └────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────┘
```

### Reusable Code

| Crate | Reuse % | Notes |
|-------|---------|-------|
| tcfs-chunks | 100% | Pure computation, no platform deps |
| tcfs-crypto | 100% | Pure computation, no platform deps |
| tcfs-storage | 70% | OpenDAL S3 works on iOS; health checks need adaptation |
| tcfs-sync | 80% | State cache and vector clocks reusable; NATS client needs iOS networking adaptation |
| tcfs-core | 100% | Proto types, config structs |

### New Code

| Component | LOC (est.) | Language |
|-----------|-----------|----------|
| UniFFI bindings | ~1000 | Rust |
| Swift FileProviderExtension | ~2000 | Swift |
| Xcode project / build scripts | ~500 | Various |

## Hydration Pattern

The hydration flow mirrors tcfs-fuse and tcfs-cloudfilter:

```
User taps file in Files app
       │
       ▼
NSFileProviderExtension.startProvidingItem(at:completionHandler:)
       │
       ▼
tcfs_file_provider::hydrate(item_id)
       │
       ├── 1. Fetch manifest from S3: manifests/{file_hash}
       ├── 2. Parse SyncManifest v2 (JSON)
       ├── 3. Fetch chunks in parallel: chunks/{chunk_hash}
       ├── 4. Decrypt chunks (XChaCha20-Poly1305)
       ├── 5. Decompress chunks (zstd)
       └── 6. Reassemble and write to provided URL
       │
       ▼
completionHandler(nil)  // success
```

### Platform Analogs

| Concept | Linux (tcfs-fuse) | Windows (tcfs-cloudfilter) | iOS (tcfs-file-provider) |
|---------|-------------------|---------------------------|--------------------------|
| Integration point | FUSE kernel module | Cloud Files minifilter | NSFileProviderExtension |
| Stub/placeholder | `.tc` file | CFAPI placeholder | NSFileProviderItem |
| Hydration trigger | `read()` syscall | `CF_CALLBACK_FETCH_DATA` | `startProvidingItem()` |
| Dehydration | `unsync` command | `CfDehydratePlaceholder` | `itemChanged(at:)` |
| Directory listing | `readdir()` | `CfGetPlaceholders` | `enumerator(for:)` |

## UniFFI Interface Definition

```udl
namespace tcfs_file_provider {
    // Initialize the provider with S3 credentials
    [Throws=ProviderError]
    void initialize(ProviderConfig config);

    // List files at a given path
    [Throws=ProviderError]
    sequence<FileItem> list_items(string path);

    // Hydrate a file (download + decrypt + decompress)
    [Throws=ProviderError]
    void hydrate_file(string item_id, string destination_path);

    // Upload a local file
    [Throws=ProviderError]
    void upload_file(string local_path, string remote_path);

    // Get sync status
    [Throws=ProviderError]
    SyncStatus get_sync_status();
};

dictionary ProviderConfig {
    string s3_endpoint;
    string s3_bucket;
    string access_key;
    string secret_key;
    string remote_prefix;
    string? encryption_key;
};

dictionary FileItem {
    string item_id;
    string filename;
    u64 file_size;
    i64 modified_timestamp;
    boolean is_directory;
    string content_hash;
};

dictionary SyncStatus {
    boolean connected;
    u64 files_synced;
    u64 files_pending;
    string? last_error;
};

[Error]
enum ProviderError {
    "StorageError",
    "DecryptionError",
    "NetworkError",
    "NotFound",
    "PermissionDenied",
};
```

## Phase Roadmap

### Phase 7a: UniFFI Bindings (COMPLETE)

- RFC document (this file)
- `tcfs-file-provider` crate with `uniffi` feature flag
- UniFFI proc-macro bindings (`uniffi_bridge.rs`, ~300 LOC)
- Exposes: `TcfsProviderHandle`, `list_items`, `hydrate_file`, `upload_file`, `delete_item`, `create_directory`, `get_sync_status`
- Error types: `ProviderError` enum with Storage/Decryption/Network/NotFound/Conflict
- Config: `ProviderConfig` record (S3 creds + E2EE passphrase)
- Uses `direct` backend (no gRPC — iOS sandbox blocks UDS)
- Compiles for host target; iOS cross-compilation next phase

### Phase 7b: iOS Project Scaffold (IN PROGRESS)

- [x] `FileProviderExtension.swift` — read path plus experimental create, modify, and delete hooks via UniFFI
- [x] `FileProviderEnumerator.swift` — directory listing via `provider.listItems(path:)`
- [x] `FileProviderItem.swift` — NSFileProviderItem with placeholder support
- [x] `HostApp.swift` — SwiftUI app with credential config + domain registration
- [x] iOS plists (Extension-Info.plist, HostApp-Info.plist) targeting iOS 17+
- [x] Entitlements (shared Keychain access group `group.io.tinyland.tcfs`)
- [x] `build-ios.sh` — cargo cross-compile + UniFFI bindgen + xcodebuild
- [x] Verified: Rust staticlib builds for `aarch64-apple-ios-sim`
- [x] `project.yml` — xcodegen spec for programmatic project generation
- [x] CI job: `ios-typecheck` validates Swift against iOS SDK on every push
- [ ] Xcode project (.xcodeproj) — generate via `xcodegen` or create in Xcode
- [ ] Test on iOS Simulator (needs runtime download)

### Phase 7c: E2E Encryption (COMPLETE — shipped with 7a/7b)

- [x] tcfs-crypto wired through UniFFI (XChaCha20-Poly1305 + Argon2id KDF)
- [x] Keychain credential storage (encryption_passphrase + encryption_salt)
- [x] Encrypted hydration flow (file key unwrap → chunk decrypt → decompress)
- [x] Encrypted upload flow (chunk compress → encrypt → file key wrap)

### Phase 7d: Sync Engine

- Wire tcfs-sync state cache through UniFFI
- Background refresh via `NSFileProviderManager.signalEnumerator`
- Push notifications for real-time updates (APNs or polling)

### Phase 7e: UI + Polish (PR #65, IN PROGRESS)

- [x] Progress reporting during hydration (UniFFI callback interface → NSProgress)
- [x] Conflict detection via vclock divergence (`check_conflict_async`)
- [x] Sync status dashboard in host app (live file count + error display)
- [x] Host app type-checks with UniFFI bindings
- [ ] Share extension for uploading
- [ ] Manual TestFlight beta path (requires Apple Developer Program enrollment; not a continuously proven release lane)

## Technical Challenges

### Async FFI

UniFFI supports async functions but the bridge between tokio (Rust) and
Swift concurrency (async/await) requires careful lifetime management.
The recommended pattern is to run a tokio runtime in the Rust layer and
expose blocking or callback-based APIs to Swift.

```rust
// Rust side: run async in dedicated runtime
static RUNTIME: Lazy<Runtime> = Lazy::new(|| Runtime::new().unwrap());

#[uniffi::export]
fn hydrate_file(item_id: String, dest: String) -> Result<(), ProviderError> {
    RUNTIME.block_on(async {
        // ... async hydration logic
    })
}
```

### Keychain Credentials

iOS sandbox prevents reading env vars or config files from the host.
Credentials must be stored in the iOS Keychain and accessed via
`Security.framework`:

```swift
let query: [String: Any] = [
    kSecClass: kSecClassGenericPassword,
    kSecAttrService: "com.tummycrypt.tcfsd",
    kSecAttrAccount: "s3_access_key",
    kSecReturnData: true,
]
```

### App Sandbox Restrictions

- File Provider extensions run in a separate process with limited memory (~50 MB)
- No direct filesystem access outside the extension's container
- Network requests must use `URLSession` (not raw sockets)
- OpenDAL's S3 backend uses `reqwest` which should work via iOS networking

### OpenDAL iOS Compilation

OpenDAL with `services-s3` feature needs:
- Cross-compilation to `aarch64-apple-ios`
- Ring (TLS dependency) compiles for iOS with proper SDK paths
- Tested: OpenDAL 0.55 builds for iOS targets

## Dependency Chain

```
tcfs-core (proto types, config)
    │
    ├── tcfs-storage (OpenDAL S3 operator)
    │       │
    │       └── tcfs-chunks (FastCDC + BLAKE3 + zstd)
    │               │
    │               └── tcfs-crypto (XChaCha20 + Argon2id)
    │
    └── tcfs-sync (state cache, vector clocks)
            │
            └── tcfs-file-provider (UniFFI bridge) ← NEW
                    │
                    └── Swift FileProviderExtension (Xcode project) ← FUTURE
```

## References

- [Apple File Provider documentation](https://developer.apple.com/documentation/fileprovider)
- [Mozilla UniFFI](https://mozilla.github.io/uniffi-rs/)
- [tcfs-cloudfilter](../../crates/tcfs-cloudfilter/) — Windows analog
- [tcfs-fuse](../../crates/tcfs-fuse/) — Linux analog

---

Signed-off-by: xoxd
