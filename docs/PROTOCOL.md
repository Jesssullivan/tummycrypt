# tcfs Protocol Specification

The full protocol specification is maintained as a LaTeX source file and
distributed as PDF.

- **Source**: [`docs/tex/protocol.tex`](tex/protocol.tex)
- **PDF**: Built by CI and available as a [release artifact](https://github.com/Jesssullivan/tummycrypt/actions/workflows/docs.yml)

To build locally:

```bash
task docs:pdf
# Output: dist/docs/protocol.pdf
```

## Quick Reference

See the [Protocol PDF](https://github.com/Jesssullivan/tummycrypt/actions/workflows/docs.yml) for full details including:

- Physical `.tc` file stub format (sorted key/value text)
- Physical `.tcf` folder stub format
- Remote index layout used by mounted views and FileProvider enumeration
- S3 chunk layout (content-addressed storage)
- Chunk manifest format
- FastCDC chunking parameters
- Hydration flow
- The distinction between physical stubs, mounted clean-name entries, and platform placeholders
- State tracking schema
- gRPC wire protocol (11 RPCs, including `ResolveConflict`)
- NATS `StateEvent` types: `FileSynced`, `FileDeleted`, `FileRenamed`, `DeviceOnline`, `DeviceOffline`, `ConflictResolved`
- NATS subject hierarchy: `STATE.{device_id}.{event_type}`
- SyncManifest v2 JSON format (with v1 text fallback)
