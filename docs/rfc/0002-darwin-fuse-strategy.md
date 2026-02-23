# RFC 0002: Darwin File Integration Strategy

**Status**: Draft (Revised)
**Author**: xoxd
**Date**: 2026-02-23
**Tracking**: Sprint 7+
**Supersedes**: Initial draft (FUSE-only framing)

---

## Abstract

This RFC addresses macOS file integration for tcfs. The initial draft evaluated
FUSE alternatives (macFUSE, FUSE-T, NFS loopback, FSKit). Research revealed that
**Apple's FileProvider framework (`NSFileProviderReplicatedExtension`)** is the
correct integration point for cloud storage on macOS --- the same mechanism used
by iCloud Drive, Dropbox, and OneDrive.

FUSE remains the correct approach for Linux. macFUSE serves as a transitional
option on macOS for users who want it, but FileProvider is the primary path.

## Motivation

During Sprint 6 fleet deployment, FUSE mounting failed on both Darwin machines:

| Host | macOS Version | Error | Root Cause |
|------|---------------|-------|------------|
| xoxd-bates | Sequoia 15.3 | `Read-only file system (os error 30)` | macFUSE system extension not approved |
| petting-zoo-mini | Sequoia 15.3 | `Permission denied (os error 13)` | macFUSE system extension not loaded |

Investigation of alternatives (FUSE-T, NFS loopback, FSKit) revealed fundamental
limitations. FileProvider emerged as the Apple-sanctioned solution designed
specifically for cloud storage with on-demand hydration.

## Platform Integration Matrix

| Platform | Integration Point | Framework | Crate | Status |
|----------|------------------|-----------|-------|--------|
| Linux | Kernel FUSE | fuse3 | tcfs-fuse | Working (v0.3.0) |
| Windows | Cloud Files API | CFAPI | tcfs-cloudfilter | Skeleton |
| macOS | FileProvider | NSFileProviderReplicatedExtension | tcfs-fileprovider | This RFC |
| iOS | FileProvider | NSFileProviderExtension | tcfs-fileprovider (shared) | RFC 0003 |

Note: **macOS and iOS share the FileProvider framework.** The `tcfs-fileprovider`
crate serves both platforms with platform-conditional compilation.

---

## Part 1: Why Not FUSE on macOS

### FUSE Options Evaluated

| Approach | Blocker |
|----------|---------|
| **macFUSE (kext)** | Requires system extension approval, Reduced Security on Apple Silicon, fragile across macOS upgrades |
| **macFUSE (FSKit backend)** | macOS 15.4+ only; mount restricted to `/Volumes`; FUSE notifications not supported; non-local FS deferred to macOS 26 |
| **FUSE-T** | libfuse2 only (tcfs uses fuse3 crate); no FSEvents; no Spotlight; Finder crash on copy (macOS 15, issue #75) |
| **NFS loopback** | `mount(2)` requires root on macOS (kernel restriction); no FSEvents; no Spotlight |
| **FSKit native** | Block devices only; no cloud/network filesystem support; `FSFileSystem` not public in v1; non-local FS deferred to macOS 26 |

### Key Findings

1. **`mount(2)` requires root on macOS** --- this is a kernel-level restriction.
   The `diskarbitrationd` daemon runs as root specifically for this. macOS does
   not support Linux's `user` fstab option. This eliminates NFS loopback's
   "zero-privilege" advantage.

2. **FSKit is for disk filesystems, not cloud storage.** It operates on
   `FSBlockDeviceResource` (block devices). There is no `FSResource` type for
   network/cloud endpoints. The Cryptomator project confirmed FSKit is "not
   suitable" for their encrypted cloud storage use case.

3. **FUSE-T only supports libfuse2.** The `fuse3` Rust crate used by tcfs-fuse
   is not compatible. An open FUSE-T issue (#93) requests fuse3 support with no
   timeline.

4. **FSEvents only work with macFUSE kext backend.** All NFS-based approaches
   (FUSE-T, raw loopback) lose Finder change notifications and Spotlight indexing.

5. **macFUSE 5.x has an FSKit backend** (`-o backend=fskit`), but it is
   experimental: mount restricted to `/Volumes`, FUSE notifications not yet
   supported, non-local (distributed) volumes deferred to macOS 26.

---

## Part 2: FileProvider --- The Correct Approach

### What is FileProvider?

`NSFileProviderReplicatedExtension` (macOS 11+) is Apple's official framework
for cloud storage integration. It provides:

| Feature | Support |
|---------|---------|
| Files in Finder sidebar | Yes, under "Locations" at `~/Library/CloudStorage/` |
| On-demand hydration | Yes, via APFS dataless files |
| Cloud status icons | Yes (downloaded, cloud-only, syncing) |
| Spotlight integration | Yes (native APFS) |
| FSEvents integration | Yes (native APFS) |
| File pinning | Yes (macOS Sonoma+) |
| Automatic eviction | Yes (system reclaims space when disk is low) |
| Incremental fetch | Yes (`NSFileProviderIncrementalContentFetching`) |
| Conflict resolution | Built-in version tracking |
| Minimum macOS | 11.0 (Big Sur) |
| Kernel extension | None required |
| System extension approval | None required |
| Root/sudo | None required |

### How Hydration Works

When a user opens a cloud-only file, the kernel intercepts the read and calls
the FileProvider extension:

```
User opens file in Finder / CLI
       |
       v
macOS VFS (APFS dataless file detected)
       |
       v
Kernel pauses the read, calls FileProvider extension
       |
       v
NSFileProviderReplicatedExtension.fetchContents(
    for: itemIdentifier,
    version: currentVersion,
    request: NSFileProviderRequest,
    completionHandler: (URL?, NSFileProviderItem?, Error?) -> Void
)
       |
       v
tcfs-fileprovider Rust backend:
  1. Fetch manifest from S3 (manifests/{file_hash})
  2. Fetch chunks in parallel (chunks/{chunk_hash})
  3. Decrypt chunks (XChaCha20-Poly1305)
  4. Decompress chunks (zstd)
  5. Write reassembled file to provided temporary URL
       |
       v
completionHandler(temporaryFileURL, updatedItem, nil)
       |
       v
Kernel delivers file content to the application
```

This is identical to how iCloud Drive, Dropbox, and OneDrive work.

### Architecture

```
[Finder / CLI / Any App]
       |
       v
[macOS Kernel -- APFS dataless files]
       |  (on-demand hydration callback)
       v
[FileProvider Extension (.appex)]     ~200 LOC Swift
       |  extern "C" function calls
       v
[tcfs-fileprovider (Rust static lib)]
       |  uses existing tcfs crates
       v
[tcfs-chunks]  [tcfs-crypto]  [tcfs-storage]  [tcfs-sync]
       |
       v
[SeaweedFS S3 / NATS JetStream]
```

### Swift Shim (Minimal)

The FileProvider extension entry point must be Swift or Objective-C. The shim is
approximately 200 lines implementing:

| Protocol / Class | Methods | LOC (est.) |
|-----------------|---------|------------|
| `NSFileProviderReplicatedExtension` | `init(domain:)`, `invalidate()`, `item(for:)`, `fetchContents(for:)`, `enumerator(for:)` | ~50 |
| `NSFileProviderItem` | Returns identifier, parent, filename, type, size, version | ~30 |
| `NSFileProviderEnumerator` | `enumerateItems(for:startingAt:)`, `invalidate()` | ~40 |
| C bridge declarations | `extern` function declarations for Rust backend | ~30 |

All actual logic (S3 operations, chunking, encryption, sync) remains in Rust.
The Swift shim is a pure translation layer.

### Rust Backend Design

The `tcfs-fileprovider` crate compiles as a `staticlib` exposing C-compatible
functions:

```rust
// Exported C API (auto-generated header via cbindgen)
#[no_mangle]
pub extern "C" fn tcfs_fp_enumerate_dir(
    path: *const c_char,
    out_items: *mut *mut FPItem,
    out_count: *mut usize,
) -> i32 { ... }

#[no_mangle]
pub extern "C" fn tcfs_fp_fetch_contents(
    item_id: *const c_char,
    dest_path: *const c_char,
    progress_cb: extern "C" fn(f64),
) -> i32 { ... }

#[no_mangle]
pub extern "C" fn tcfs_fp_get_item_metadata(
    item_id: *const c_char,
    out_item: *mut FPItem,
) -> i32 { ... }
```

Internal dependencies (already written):
- `tcfs-storage` --- S3/SeaweedFS access via OpenDAL
- `tcfs-chunks` --- FastCDC chunking, BLAKE3 hashing, zstd compression
- `tcfs-crypto` --- XChaCha20-Poly1305 encryption
- `tcfs-sync` --- Vector clocks, state cache, conflict detection

### Build Pipeline

```
cargo build --lib --target aarch64-apple-darwin  (Rust staticlib)
       |
       v
cbindgen --lang c  (generate tcfs_fileprovider.h)
       |
       v
Xcode project:
  - Host app (minimal, manages extension lifecycle)
  - FileProvider extension target (.appex)
    - Links Rust static library
    - Imports C header via module map
    - Swift shim implements NSFileProviderReplicatedExtension
       |
       v
codesign + notarytool  (Developer ID signing + notarization)
       |
       v
Distribution: .dmg installer or Homebrew cask
```

### Credential Storage

iOS/macOS sandbox prevents reading env vars or config files from the host.
FileProvider extensions use:

1. **App Group container** --- shared preferences between host app and extension
2. **Keychain** --- S3 credentials stored in a shared Keychain access group
3. **App Group UserDefaults** --- configuration (endpoint URL, bucket, prefix)

---

## Part 3: Transitional macFUSE Support

For users who prefer FUSE mount semantics or run older macOS versions without
FileProvider, macFUSE remains available as an opt-in backend.

### ABR (Admin By Request) Integration

On managed machines, macFUSE requires privilege escalation for system extension
approval. The daemon detects ABR and adapts:

```
if macFUSE available && extension approved:
    mount via macFUSE (full FUSE semantics)
elif FileProvider available:
    register FileProvider domain (recommended)
else:
    run in degraded mode (CLI push/pull only, no mount)
```

### Launchd Plist

macOS daemon startup via launchd (for both FileProvider and FUSE modes):

```
dist/com.tummycrypt.tcfsd.plist
  - RunAtLoad: true
  - KeepAlive: true
  - Logs: /tmp/tcfsd.stdout.log, /tmp/tcfsd.stderr.log
```

See `docs/ops/fleet-deployment.md` for installation instructions.

---

## Part 4: FUSE-T Assessment

FUSE-T deserves mention as a kext-free alternative that uses NFSv4 loopback
internally. Key findings:

| Aspect | Detail |
|--------|--------|
| Mechanism | Userspace NFSv4 server on ephemeral port |
| Kext/sysext | None required |
| Root for mount | Not per-mount (installer needs root once) |
| API | **libfuse2 only** --- fuse3 not supported (issue #93) |
| FSEvents | Not supported (NFS loopback limitation) |
| Spotlight | Not supported |
| Notable users | rclone, Cryptomator, VeraCrypt |
| Status | Active, v1.0.49 (Aug 2025), sole maintainer |
| macOS 15 | Known Finder crash on file copy (issue #75) |

**FUSE-T is not viable for tcfs** because tcfs-fuse uses the `fuse3` Rust crate.
FUSE-T only implements libfuse2. Switching to a fuse2-compatible crate (`fuser`)
would require rewriting tcfs-fuse's entire callback layer.

FUSE-T validates the NFS loopback approach architecturally, but the FileProvider
path is superior in every dimension (FSEvents, Spotlight, no root, native Finder
integration, APFS dataless files).

---

## Part 5: FSKit Status and Future

FSKit (macOS 15.4+) is Apple's framework for custom *disk* filesystems. It is
not currently suitable for tcfs but may be relevant in the future.

### Current State

| Aspect | Status |
|--------|--------|
| Public API | `FSUnaryFileSystem` only; `FSFileSystem` marked `FSKIT_API_UNAVAILABLE_V1` |
| Cloud/network FS | **Not supported** (block devices only) |
| Non-local FS | Deferred to **macOS 26** |
| Mount restriction | `/Volumes` only |
| Entitlement | `com.apple.developer.fskit.fsmodule` |
| Extension type | Application extension (.appex), not system extension |
| User approval | Required in System Settings > Extensions |
| Third-party adoption | One sample project (KhaosT/FSKitSample); no production use |

### macFUSE 5.x FSKit Backend

macFUSE 5.0+ includes an experimental FSKit backend (`-o backend=fskit`):

| Feature | FSKit Backend | Kext Backend |
|---------|--------------|--------------|
| Mount points | `/Volumes` only | Anywhere |
| FUSE notifications | Not supported | Supported |
| Minimum macOS | 15.4 | 12+ |
| Non-local volumes | macOS 26+ | Now |
| libfuse3 | macFUSE 5.1.0+ | macFUSE 5.1.0+ |

### Future Relevance

If Apple extends FSKit to support non-local filesystems in macOS 26+, it could
become an alternative to FileProvider for tcfs. However, FileProvider is the
established, production-proven path with 5+ years of maturity and broad macOS
version support.

---

## Part 6: Language Choice for Platform Layer

The initial draft proposed Zig as an alternative for the platform-specific layer.
Research found this is not viable for the FileProvider use case:

| Factor | Zig | Rust |
|--------|-----|------|
| ObjC header import | `@cImport` cannot parse ObjC syntax | `objc2` covers 16+ Apple frameworks |
| .appex bundle output | Cannot produce MH\_BUNDLE Mach-O | Cargo + Xcode works |
| FileProvider bindings | None exist | swift-bridge, cbindgen, UniFFI |
| FSKit bindings | None exist | fskit-rs, objc2-fs-kit |
| Existing codebase | Would add a 2nd systems language | Already 100% Rust |

Zig's advantage (simpler C ABI, smaller binaries) would matter for a raw
block-device filesystem driver. For a cloud storage client calling Apple
frameworks through a Swift shim, Rust + cbindgen is the pragmatic choice.

---

## Implementation Roadmap

### Phase 1: Transitional (v0.4.x)

- macOS daemon starts in `--no-mount` degraded mode by default
- Push/pull/sync work fully (S3 + NATS, no FUSE required)
- macFUSE available as opt-in for users who install it
- ABR detection and guided setup for managed machines
- Launchd plist for automatic startup

### Phase 2: FileProvider MVP (v0.5.x)

- `tcfs-fileprovider` crate: Rust `staticlib` with C API
- Minimal Xcode project with FileProvider extension
- Swift shim (~200 LOC) implementing `NSFileProviderReplicatedExtension`
- Read-only enumeration + on-demand hydration
- Keychain credential storage
- Apple Developer ID enrollment for code signing

### Phase 3: FileProvider Full (v0.6.x)

- Write support (upload via `createItem` / `modifyItem`)
- Conflict resolution via `NSFileProviderItemVersion`
- Background sync via `NSFileProviderManager.signalEnumerator`
- Progress reporting during hydration
- File pinning support
- E2E encryption through the hydration path

### Phase 4: Polish (v1.0)

- Automatic eviction policy integration
- Thumbnail / QuickLook preview generation
- Share extension
- Homebrew cask distribution
- Deprecate macFUSE transitional path

---

## Open Questions

1. **Apple Developer enrollment**: Is Tinyland Inc already enrolled in the Apple
   Developer Program ($99/year)? If not, who initiates enrollment?

2. **App Group identifier**: What App Group and Keychain access group should we
   register? Proposed: `group.com.tummycrypt.tcfs`

3. **Distribution format**: .dmg with drag-to-install? Homebrew cask? Both?

4. **macOS minimum version**: FileProvider is available since macOS 11, but
   `NSFileProviderReplicatedExtension` matured significantly in macOS 13+.
   Recommend targeting macOS 13 (Ventura) as minimum.

---

## References

- [Apple FileProvider documentation](https://developer.apple.com/documentation/fileprovider)
- [NSFileProviderReplicatedExtension](https://developer.apple.com/documentation/fileprovider/nsfileproviderreplicatedextension)
- [WWDC 2021: Sync files to the cloud with FileProvider on macOS](https://developer.apple.com/videos/play/wwdc2021/10182/)
- [Apple FSKit documentation](https://developer.apple.com/documentation/fskit)
- [FUSE-T](https://github.com/macos-fuse-t/fuse-t)
- [macFUSE FSKit backend](https://github.com/macfuse/macfuse/wiki/FUSE-Backends)
- [fskit-rs](https://github.com/debox-network/fskit-rs) --- Rust FSKit bindings
- [KhaosT/FSKitSample](https://github.com/KhaosT/FSKitSample)
- tcfs-cloudfilter: `crates/tcfs-cloudfilter/` --- Windows analog
- tcfs-fuse: `crates/tcfs-fuse/` --- Linux FUSE3 implementation

---

Signed-off-by: xoxd
