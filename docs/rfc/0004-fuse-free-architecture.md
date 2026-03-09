# RFC 0004: FUSE-Free Filesystem Architecture

**Status**: Draft
**Author**: xoxd
**Date**: 2026-03-09
**Tracking**: Epic 1 Track D / Epic 2
**Supersedes**: RFC 0002 (FUSE as Linux default), FUSE-T migration (PR #67)

---

## Abstract

This RFC proposes eliminating FUSE from all tummycrypt platforms. FUSE has been a
persistent source of deployment friction — macFUSE requires a kernel extension that
users must manually approve, FUSE-T works but adds an unnecessary abstraction layer
over NFS, and both require third-party libraries that complicate packaging.

The replacement architecture uses three platform-native backends:

| Platform | Backend | Kernel Module? | Mount Path |
|----------|---------|----------------|------------|
| macOS | FileProvider (APFS dataless) | No | `~/Library/CloudStorage/TummyCrypt/` |
| iOS | FileProvider (existing) | No | Files.app integration |
| Linux | NFS loopback (`nfsserve`) | No | User-chosen mount point |

All three share a new `VirtualFilesystem` trait extracted from the existing FUSE
driver, ensuring a single codebase for filesystem logic.

## Motivation

### FUSE deployment failures (Sprint 6–8)

| Host | Platform | FUSE Variant | Failure Mode |
|------|----------|-------------|--------------|
| xoxd-bates | macOS Sequoia | macFUSE | `Read-only file system` — SysExt not approved |
| xoxd-bates | macOS Sequoia | FUSE-T | Daemon mounts invisible — subprocess env missing |
| petting-zoo-mini | macOS Sequoia | macFUSE | `Permission denied` — SysExt not loaded |
| petting-zoo-mini | macOS Sequoia | FUSE-T | Wrapper script never detected libfuse-t |

Every Darwin FUSE attempt has required manual intervention (SysExt approval, TCC
dialogs, plist patching). This is incompatible with fleet-scale deployment and
App Store submission.

### FUSE is already optional in the codebase

Only `tcfs-fuse` (1571 lines across 5 files) depends on `fuse3`. Every other crate —
`tcfs-storage`, `tcfs-sync`, `tcfs-chunks`, `tcfs-crypto`, `tcfs-file-provider` — is
completely FUSE-independent. The daemon (`tcfsd`) doesn't use FUSE directly; it spawns
`tcfs mount` as a subprocess via gRPC.

### Industry trend: FUSE is legacy on macOS

All major cloud storage providers migrated away from FUSE:

- **Dropbox**: FileProvider (2023)
- **OneDrive**: FileProvider (2022)
- **Google Drive**: FileProvider (2023)
- **iCloud Drive**: FileProvider (always)

Apple deprecated kernel extensions (kexts) in macOS 11. System extensions for FUSE
require user approval in Security settings, creating an adoption barrier.

### FUSE-T proves NFS works

FUSE-T's architecture is: userspace → libfuse-t → NFSv3 server → kernel NFS client.
PR #67 proved this works for tummycrypt. But FUSE-T adds two unnecessary layers
(libfuse-t + FUSE API translation). We can speak NFS directly.

## Design

### New crate: `tcfs-nfs`

An embedded NFSv3 server using the [`nfsserve`](https://crates.io/crates/nfsserve)
crate (Rust, tokio, ~2.5 KLOC). This is the same approach proven by XetHub for
production git-xet mounts.

```
┌──────────────────────────────────────────────┐
│  Application (cat, vim, ls, etc.)            │
├──────────────────────────────────────────────┤
│  Kernel VFS                                  │
├──────────────────────────────────────────────┤
│  Kernel NFS client (built into macOS/Linux)  │
├──────────────────────────────────────────────┤
│  localhost:2049 (TCP)                        │
├──────────────────────────────────────────────┤
│  tcfs-nfs (NFSv3 server)                     │
│  ├─ VirtualFilesystem trait impl             │
│  ├─ DiskCache (LRU, same as tcfs-fuse)       │
│  └─ Operator (OpenDAL → S3/SeaweedFS)        │
└──────────────────────────────────────────────┘
```

**Key advantages over FUSE:**
- Zero kernel modules on any platform
- NFS client is built into macOS and Linux kernels
- Battle-tested protocol (30+ years)
- Supports Linux `noacl,nolock` for simplicity
- macOS supports `mount_nfs -o resvport,locallocks`

**Mount command** (spawned by daemon):
```bash
# macOS
mount_nfs -o tcp,resvport,locallocks,vers=3 localhost:/tcfs /path/to/mount

# Linux
mount -t nfs -o tcp,noacl,nolock,vers=3 localhost:/tcfs /path/to/mount
```

**Privilege requirement**: `mount` requires root. Solutions:
1. `tcfsd` uses a setuid helper (like `fusermount3`)
2. macOS: `Security.framework` authorization prompt (one-time)
3. Linux: polkit rule for `org.freedesktop.udisks2.filesystem-mount`
4. NixOS/systemd: `mount.nfs` with `user` option in fstab

### VirtualFilesystem trait extraction

Extract the filesystem logic from `tcfs-fuse/driver.rs` into a shared trait:

```rust
// crates/tcfs-vfs/src/lib.rs

/// Platform-agnostic virtual filesystem operations.
///
/// Implementors: TcfsFuse (legacy), TcfsNfs, FileProvider DirectBackend
#[async_trait]
pub trait VirtualFilesystem: Send + Sync {
    /// File/directory attributes (stat)
    async fn getattr(&self, path: &Path) -> Result<FileAttr>;

    /// Directory listing
    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    /// Lookup child in parent directory
    async fn lookup(&self, parent: &Path, name: &OsStr) -> Result<FileAttr>;

    /// Read file contents (with offset + size for partial reads)
    async fn read(&self, path: &Path, offset: u64, size: u32) -> Result<Bytes>;

    /// Hydrate a stub file (fetch from remote, cache locally)
    async fn hydrate(&self, path: &Path) -> Result<()>;

    /// Write file contents (for sync-back)
    async fn write(&self, path: &Path, offset: u64, data: &[u8]) -> Result<u32>;

    /// Create file
    async fn create(&self, parent: &Path, name: &OsStr) -> Result<FileAttr>;

    /// Remove file
    async fn unlink(&self, parent: &Path, name: &OsStr) -> Result<()>;

    /// Create directory
    async fn mkdir(&self, parent: &Path, name: &OsStr) -> Result<FileAttr>;
}
```

**Existing code that maps to this trait:**

| Trait Method | tcfs-fuse/driver.rs | FileProvider/direct.rs |
|-------------|--------------------|-----------------------|
| `getattr` | `PathFilesystem::getattr` (L251) | `tcfs_provider_enumerate` (stat portion) |
| `readdir` | `PathFilesystem::readdir` (implicit in lookup) | `tcfs_provider_enumerate` |
| `lookup` | `PathFilesystem::lookup` (L306) | N/A (enumerate returns children) |
| `read` | `PathFilesystem::read` (via open+read) | `tcfs_provider_fetch` |
| `hydrate` | `fetch_cached()` in hydrate.rs | `tcfs_provider_fetch` |
| `write` | `PathFilesystem::write` (if implemented) | `tcfs_provider_upload` |

### FileProvider as primary macOS/iOS backend

FileProvider is already working (iOS TestFlight deployed, macOS .appex exists).
With the `VirtualFilesystem` trait, the FileProvider extension's `DirectBackend`
becomes a thin adapter:

```swift
// FileProviderExtension.swift (existing)
func fetchContents(for itemIdentifier: ...) async throws -> ... {
    // Already calls tcfs_provider_fetch → Operator → S3
    // No changes needed — this IS the FUSE-free path
}
```

**macOS FileProvider advantages:**
- APFS dataless files: metadata on disk, content hydrated on first access
- Finder integration: badges, progress bars, context menus
- System-managed eviction: OS reclaims space under disk pressure
- No mount command needed: appears in `~/Library/CloudStorage/`

**Limitation**: Fixed path (`~/Library/CloudStorage/TummyCrypt-<account>/`).
For users who need arbitrary mount points, NFS loopback is the fallback.

### Linux: fanotify pre-content hooks (future)

Linux 6.14+ introduces `FAN_PRE_CONTENT` fanotify events. When a process reads a
stub file, the kernel pauses the read, notifies the tcfs daemon, which hydrates the
file, then the kernel resumes the read. Zero-overhead for already-hydrated files.

This is the ideal long-term Linux backend but requires kernel 6.14+ (shipped mid-2025,
widespread by late 2026). NFS loopback works on all Linux kernels today.

```
Future Linux backend priority:
1. fanotify pre-content (if kernel >= 6.14) — zero overhead
2. NFS loopback (universal fallback) — ~1ms per open()
```

## Implementation Plan

### Phase 1: Extract VirtualFilesystem trait (1 sprint)

1. Create `crates/tcfs-vfs/` with the trait definition
2. Refactor `TcfsFs` in `tcfs-fuse/driver.rs` to implement the trait
3. Move `DiskCache`, `NegativeCache`, `hydrate.rs`, `stub.rs` into `tcfs-vfs`
   (these are already FUSE-independent)
4. `tcfs-fuse` becomes a thin adapter: fuse3 PathFilesystem → VirtualFilesystem

**Risk**: Low. This is a pure refactor with no behavioral changes.

### Phase 2: Build `tcfs-nfs` crate (1–2 sprints)

1. Add `nfsserve` dependency
2. Implement NFSv3 `LOOKUP`, `GETATTR`, `READDIR`, `READ`, `WRITE` handlers
   that delegate to `VirtualFilesystem`
3. Mount helper (setuid or polkit-based)
4. Integration tests: mount → read → write → unmount cycle
5. Wire into `tcfsd` as alternative to FUSE mount

**Estimated size**: ~800 lines (nfsserve handles protocol, we implement handlers)

### Phase 3: Deprecate FUSE (1 sprint)

1. Make NFS the default mount backend (`tcfs mount` uses NFS, `tcfs mount --fuse` for legacy)
2. Update daemon wrapper in `tummycrypt.nix` — no more FUSE-T/macFUSE detection
3. Remove `fuse3` from default features in `tcfs-fuse/Cargo.toml`
4. Update fleet deployment: remove FUSE-T installation from Nix config
5. Update E2E tests to validate NFS mounts

### Phase 4: FileProvider as primary macOS path (parallel with Phase 2)

1. Complete macOS FileProvider domain registration (already scaffolded)
2. Wire FileProvider's `DirectBackend` to use `VirtualFilesystem` trait
3. `tcfsd` on macOS: start FileProvider domain instead of spawning mount
4. NFS loopback available as opt-in for users who need arbitrary mount points

### Phase 5: fanotify backend (future, Linux 6.14+)

1. Create `tcfs-fanotify` crate
2. Detect kernel version at runtime
3. Prefer fanotify over NFS when available
4. Stub files become real sparse files with xattr markers

## Crate dependency graph (target state)

```
tcfs-vfs (trait + DiskCache + NegativeCache + stub + hydrate)
  ├── tcfs-storage (OpenDAL Operator)
  ├── tcfs-chunks (content-addressable store)
  └── tcfs-crypto (E2EE)

tcfs-nfs (NFSv3 server, uses tcfs-vfs)
  └── nfsserve

tcfs-fuse (LEGACY, thin adapter, uses tcfs-vfs)
  └── fuse3

tcfs-file-provider (DirectBackend, uses tcfs-vfs)
  └── UniFFI bridge → Swift

tcfs-fanotify (FUTURE, uses tcfs-vfs)
  └── fanotify-rs

tcfsd (daemon)
  ├── tcfs-nfs (default mount backend)
  ├── tcfs-fuse (--legacy flag)
  ├── tcfs-sync (push/pull/watch)
  └── tcfs-file-provider (macOS/iOS domain)
```

## Alternatives Considered

### Keep FUSE-T as default
**Rejected.** FUSE-T works but adds two unnecessary layers (libfuse-t shim + FUSE API
translation over NFS). Since we're already speaking NFS under the hood, cutting out
the middleman simplifies packaging, debugging, and deployment.

### FSKit (macOS 26 Tahoe)
**Rejected for cloud FS.** FSKit only supports `FSBlockDeviceResource` — it's designed
for formatting USB drives and disk images, not network/cloud filesystems. Apple
explicitly recommends FileProvider for cloud storage.

### WebDAV
**Rejected.** Linux WebDAV requires `davfs2` (which uses FUSE internally), defeating
the purpose. macOS WebDAV (via Finder) has poor performance and caching behavior.

### 9P / virtio-fs
**Rejected.** No macOS kernel client. Linux-only via QEMU/virtio. Not applicable.

### Plan 9 / Custom kernel module
**Rejected.** Writing kernel code is the opposite of our goal. The whole point is to
stay in userspace with zero kernel dependencies.

## Migration Impact

### Users
- **macOS**: No visible change — FileProvider already works. NFS mount replaces FUSE
  mount for terminal users who want a mount point.
- **iOS**: No change — FileProvider is already the only backend.
- **Linux**: `tcfs mount` switches from FUSE to NFS. Same UX, no kernel module needed.

### Packagers
- Remove macFUSE/FUSE-T from dependencies
- Remove `fuse3` system library requirement
- NFS client is always present (kernel built-in)

### Developers
- `tcfs-fuse` crate becomes optional (feature-gated, not in default build)
- New `tcfs-vfs` and `tcfs-nfs` crates to maintain
- FileProvider Swift code unchanged

## Open Questions

1. **NFS mount privilege**: Best approach for non-root mount? Setuid helper vs polkit
   vs fstab entry vs Security.framework prompt?
2. **Write support**: NFSv3 WRITE + NFS COMMIT for bidirectional sync, or read-only
   NFS + inotify/FSEvents for detecting local writes?
3. **Port allocation**: Fixed port (e.g., 12049) or dynamic with mount discovery?
4. **Multiple mount points**: One NFS export per sync folder, or single export with
   virtual directory tree?

## References

- [nfsserve crate](https://crates.io/crates/nfsserve) — Rust NFSv3 server library
- [XetHub git-xet](https://github.com/xetdata/xet-core) — Production NFS-based git mount
- [Apple FileProvider docs](https://developer.apple.com/documentation/fileprovider)
- [fanotify pre-content patches](https://lore.kernel.org/linux-fsdevel/) — Linux 6.14
- RFC 0002: Darwin File Integration Strategy (superseded for macOS)
- RFC 0003: iOS File Provider Extension (unchanged, complementary)
