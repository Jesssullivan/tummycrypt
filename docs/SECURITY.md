# tcfs Security Model

The full security model document is maintained as a LaTeX source file and
distributed as PDF.

- **Source**: [`docs/tex/security.tex`](tex/security.tex)
- **PDF**: Built by CI and available as a [release artifact](https://github.com/Jesssullivan/tummycrypt/actions/workflows/docs.yml)

To build locally:

```bash
task docs:pdf
# Output: dist/docs/security.pdf
```

## Overview

tcfs encrypts all file content client-side before upload using XChaCha20-Poly1305 with per-file keys derived via HKDF from a master key. The master key is protected by Argon2id key derivation with BIP-39 mnemonic recovery. Credentials are managed through a layered chain: SOPS/age encrypted files, KeePassXC databases, or environment variables. Device identity uses age keypairs with BLAKE3 fingerprints, stored in an S3-backed registry. All chunk data is content-addressed (BLAKE3) ensuring integrity verification on every read.

Credential-bearing S3 clients require an `https://` endpoint by default. The
daemon/CLI compatibility escape hatch, `storage.enforce_tls = false`, and the
macOS direct-client JSON setting, `allow_insecure_http = true`, are explicit
development/test opt-ins. The iOS UniFFI `ProviderConfig` intentionally has no
plaintext field; its lower-level test escape hatch is the process environment
variable `TCFS_STORAGE_ALLOW_INSECURE_HTTP=true`. None of these opt-ins may be
used for production or fleet reconciliation. Custom private trust roots via
`storage.ca_cert_path` are supported by daemon/CLI/core operator paths. Direct
FileProvider paths currently require a publicly WebPKI-trusted chain;
sandbox-safe private-CA/PEM propagation remains open.

## Quick Reference

See the [Security PDF](https://github.com/Jesssullivan/tummycrypt/actions/workflows/docs.yml) for full details including:

- Threat model (storage, network, client, credential threats)
- Encryption architecture (XChaCha20-Poly1305, Argon2id, HKDF)
- Key hierarchy (master, file, manifest, name keys)
- Chunk encryption pipeline
- Credential management (SOPS/age chain)
- Credential rotation (automated + manual)
- TLS configuration
- Device identity and revocation
- Security reporting
