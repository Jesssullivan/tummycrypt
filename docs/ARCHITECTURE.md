# tcfs Architecture

The full architecture document is maintained as a LaTeX source file and
distributed as PDF.

- **Source**: [`docs/tex/architecture.tex`](tex/architecture.tex)
- **PDF**: Built by CI and available as a [release artifact](https://github.com/tinyland-inc/tummycrypt/actions/workflows/docs.yml)

To build locally:

```bash
task docs:pdf
# Output: dist/docs/architecture.pdf
```

## Overview

tcfs is a Rust monorepo of 14 workspace crates organized around a daemon (`tcfsd`) that exposes 11 gRPC RPCs over a Unix domain socket. The daemon manages FUSE mounts, coordinates with SeaweedFS via OpenDAL, and synchronizes state across a device fleet using NATS JetStream with vector clocks. Clients (CLI, TUI, MCP server) connect to the daemon via gRPC. Files are content-addressed using FastCDC chunking with BLAKE3 hashes, compressed with zstd, and encrypted with XChaCha20-Poly1305 before upload.

## Quick Reference

See the [Architecture PDF](https://github.com/tinyland-inc/tummycrypt/actions/workflows/docs.yml) for full details including:

- System architecture (client + server components)
- Crate map (14 workspace crates)
- Stub file format specification
- Hydration sequence
- Credential chain
- Phase roadmap
- Infrastructure layout
