# tcfs Security Model

The full security model document is maintained as a LaTeX source file and
distributed as PDF.

- **Source**: [`docs/tex/security.tex`](tex/security.tex)
- **PDF**: Built by CI and available as a [release artifact](https://github.com/tinyland-inc/tummycrypt/actions/workflows/docs.yml)

To build locally:

```bash
task docs:pdf
# Output: dist/docs/security.pdf
```

## Quick Reference

See the [Security PDF](https://github.com/tinyland-inc/tummycrypt/actions/workflows/docs.yml) for full details including:

- Threat model (storage, network, client, credential threats)
- Encryption architecture (XChaCha20-Poly1305, Argon2id, HKDF)
- Key hierarchy (master, file, manifest, name keys)
- Chunk encryption pipeline
- Credential management (SOPS/age chain)
- Credential rotation (automated + manual)
- TLS configuration
- Device identity and revocation
- Security reporting
