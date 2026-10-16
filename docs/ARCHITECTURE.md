# tcfs Architecture

The full architecture document is maintained as a LaTeX source file and
distributed as PDF.

- **Source**: [`docs/tex/architecture.tex`](tex/architecture.tex)
- **PDF**: Built by CI and available as a [release artifact](https://github.com/Jesssullivan/tummycrypt/actions/workflows/docs.yml)

To build locally:

```bash
task docs:pdf
# Output: dist/docs/architecture.pdf
```

## Overview

tcfs is a Rust monorepo of 18 workspace crates organized around a daemon
(`tcfsd`) that exposes a gRPC service over a Unix domain socket. The daemon
manages Linux FUSE mounts, coordinates with SeaweedFS via OpenDAL, and
synchronizes state across a device fleet using NATS JetStream with vector
clocks. Clients (CLI, TUI, MCP server) connect to the daemon via gRPC. Files are
split with FastCDC and encrypted with XChaCha20-Poly1305 before upload.
When encryption is enabled (the `crypto` feature and a file key), chunks are
stored under the BLAKE3 hash of their ciphertext; otherwise under the
plaintext chunk hash.

Compression is not in the data path today. `tcfs-chunks` ships a
`seekable_zstd` module (`crates/tcfs-chunks/src/seekable_zstd.rs`), but no
sync, VFS, FUSE, FileProvider or `tcfsd` code calls it. The stub format's
`compressed` field (`crates/tcfs-vfs/src/stub.rs`) and the `compressed` and
`compressed_length` fields in `crates/tcfs-core/src/types.rs` are reserved
for it. The only zstd use in production code is compacting enrollment
payloads (`crates/tcfs-auth/src/enrollment.rs`). The LaTeX sources were
corrected to match on 2026-10-16.

Manifests and the remote index are plaintext. A manifest is JSON holding the
relative path, file size, plaintext file BLAKE3, chunk list and vector clock;
index keys are `{prefix}/index/{rel_path}`. Only chunk content is encrypted.
See [SPEC.md](SPEC.md#current-status) for this and the other gaps between the
documented security model and the code.

## Quick Reference

See the [Architecture PDF](https://github.com/Jesssullivan/tummycrypt/actions/workflows/docs.yml) for full details including:

- System architecture (client + server components)
- Crate map (18 workspace crates)
- Stub file format specification
- Hydration sequence
- Credential chain
- Phase roadmap
- Legacy deployment shape (live backend state is in
  [ops/current.md](ops/current.md))
