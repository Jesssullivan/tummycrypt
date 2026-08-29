# TCFS FileProvider post-M10 hardening: premise re-check (TIN-1547)

Date: 2026-08-28
Status: finding + forward design, not implementation. Written to close the
design half of TIN-1547 after re-verifying its premise against current
`origin/main` and finding it stale in a specific, load-bearing way.

## The premise TIN-1547 was written against is no longer current

TIN-1547 (last substantive update 2026-05-26, per Linear) assumes a
FileProvider that can already mutate the remote: its acceptance criteria ask
for rename to be "copy/upload-before-delete with rollback or explicit
unsupported behavior," for badge/progress/recovery assertions, and for one
longer desktop soak. That assumption was true as of the ticket's "Current
proven path as of 2026-05-21" list (exact hydrate, evict/rehydrate,
FileProvider mutation remote pull, FileProvider rename remote pull).

It stopped being true on **2026-07-16**, commit `5bc2175` ("feat(sync):
harden trusted stable-root pipeline (TIN-2853)"), whose own commit message
says it closes "FileProvider fail-closed gaps found during PR #551
recovery." That commit made every Apple-facing mutation entry point
explicitly read-only:

```rust
// crates/tcfs-file-provider/src/direct.rs:20
const FILE_PROVIDER_READ_ONLY_ERROR: &str =
    "TCFS FileProvider is read-only until exact version-token conditional publication is available";
```

`tcfs_provider_upload` and `tcfs_provider_delete` (the exact C FFI entry
points a rename implementation would call) both call
`self.reject_file_provider_mutation()` unconditionally on `origin/main`
today -- confirmed by reading `crates/tcfs-file-provider/src/direct.rs` and
`crates/tcfs-file-provider/src/grpc_backend.rs` directly (not inferred from
docs). The `CHANGELOG.md` `[Unreleased]` section states this in product
terms: "Apple write capabilities and callbacks plus the C, gRPC, and UniFFI
mutation entry points are explicitly read-only and reject before I/O until
an opaque exact-version conditional publication protocol is available."

The Swift extension matches: `swift/fileprovider/Sources/Extension/FileProviderExtension.swift`'s
`modifyItem` and `deleteItem` on `origin/main` are both unconditional stubs
returning `NSFileProviderError(.cannotSynchronize)` -- there is currently no
rename code path in the shipped extension to hold a rollback story at all.

## What this means for TIN-1547's acceptance criteria

- **"Rename is copy/upload-before-delete with rollback or explicit
  unsupported behavior"** -- already satisfied, in the "explicit unsupported"
  branch, and more strongly than the ticket's authors anticipated: it's not
  just unsupported, it's a deliberate fail-closed security/correctness
  posture (TIN-2853) blocking on "exact version-token conditional
  publication," not a gap waiting for someone to wire up a rename handler.
  Do not implement copy/upload-before-delete rename against
  `tcfs_provider_upload`/`tcfs_provider_delete` as they exist today -- doing
  so would either not compile against the FFI's current reject-everything
  behavior, or (if someone unstubs the reject calls without the version-token
  protocol TIN-2853 exists to require) would reopen exactly the fail-closed
  gap TIN-2853 was written to close.
- **"Badge/progress/recovery assertions are either automated or explicitly
  scoped out"** -- explicitly scoped out here, for the same reason: there is
  no mutation surface today to have progress or recovery state about. A
  badge/progress UI over a read-only extension has nothing to report beyond
  hydration state, which `docs/ops/lazy-hydration-demo.md` and
  `docs/ops/macos-fileprovider-reality.md` already cover.
- **"One longer PZM desktop soak"** -- deferred with this note, per this
  tranche's instruction for TIN-1547. A soak of mutation recovery behavior
  that doesn't exist yet would not produce meaningful evidence; a hydration-
  only soak is a different, narrower thing than what M10/M11 asked for and
  should be scoped as its own ticket if wanted before write support returns.

## Prior art worth knowing about (not on `origin/main`, not integrated here)

A local, unpushed-to-origin working branch (`facet6/dotgit-conflict-corruption-harness`,
head `f9fb683`, itself descended from an ancestor of `origin/main`) contains
a full rename implementation in `FileProviderExtension.swift`'s `modifyItem`
predating TIN-2853's read-only hardening: fetch-or-use-provided-contents,
`tcfs_provider_upload` to the new path, then `tcfs_provider_delete` the old
path, explicit `unsupported` for directory rename. That implementation has
its own real gap worth carrying into whatever eventually replaces
`FILE_PROVIDER_READ_ONLY_ERROR`: if upload succeeds but the old-path delete
fails, it logs an error and returns failure to Finder, but never rolls back
the already-uploaded new path -- the remote briefly (or indefinitely, if the
delete keeps failing) holds both the old and new paths, and nothing surfaces
that as a distinct, recoverable state to the user. Worth fixing at the same
time the read-only posture lifts, not before.

## Recommended shape for a future rename+rollback implementation

Once "exact version-token conditional publication" lands and
`tcfs_provider_upload`/`tcfs_provider_delete` stop rejecting:

1. Keep the copy/upload-before-delete order (never delete before the new
   path is confirmed durable) -- this part of the prior-art branch's shape
   is correct and should carry forward.
2. On upload success + delete failure, attempt one rollback: delete the
   *new* path to restore the pre-rename state, rather than leaving both
   paths live. If the rollback delete also fails, surface a distinct error
   class (not the generic `.serverUnreachable`) so Finder/logs can tell "old
   path still exists, remote authority is now the new path and you have a
   stray duplicate" apart from "rename failed cleanly, nothing changed."
3. Directory rename stays explicitly unsupported (matches the prior-art
   branch and is the right call independent of version-token work -- it's a
   distinct, larger problem: an atomic multi-object move).
4. Only then define the badge/progress states TIN-1547 asks for: `renaming`,
   `rename-rolled-back`, `rename-partial-manual-cleanup-needed` are the
   three terminal-ish states worth surfacing distinctly; a plain in-flight
   spinner covers everything else.

## What this document does not do

It does not implement rename, rollback, badges, or a soak. It does not
touch `crates/tcfs-file-provider` or the Swift extension -- both are
correctly read-only today per a deliberate, dated, documented decision this
pass has no basis to override. The only safe, honest contribution here was
correcting TIN-1547's premise and recording the design for whoever picks
this back up once TIN-2853's blocking condition is resolved.
