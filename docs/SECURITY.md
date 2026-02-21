# Security

## Threat Model

tcfs protects user data stored on self-hosted SeaweedFS. The primary threats are:

1. **Storage compromise** -- attacker gains read access to SeaweedFS volumes
2. **Network interception** -- attacker observes S3/NATS traffic
3. **Client compromise** -- attacker gains access to a enrolled device
4. **Credential theft** -- attacker obtains S3 access keys

### What tcfs protects against

- **At-rest data confidentiality** (with E2E encryption enabled): files are encrypted client-side before upload using XChaCha20-Poly1305. The storage operator never sees plaintext.
- **Credential exposure in memory**: S3 secret keys are held in `SecretString` (zeroized on drop) to minimize lingering secrets in process memory.
- **Credential exposure on disk**: credentials are stored in SOPS-encrypted YAML files, decrypted at runtime with age identities.
- **Password visibility**: KDBX passwords are never passed as CLI arguments; they are read interactively via `rpassword` to prevent exposure in `ps` output.

### What tcfs does NOT protect against

- **Key extraction from a running process**: a root-level attacker with ptrace access can still read decrypted keys from memory.
- **Compromised client devices**: if a device's age private key is extracted, an attacker can decrypt all files accessible to that device.
- **Denial of service**: tcfs does not defend against an attacker deleting or corrupting data on SeaweedFS.

## Encryption Architecture

### Key Hierarchy

```
User Master Key (256-bit, derived via Argon2id from passphrase)
  |
  +-- File Encryption Key (per-file, 256-bit random)
  |     Wrapped by master key using XChaCha20-Poly1305
  |     Used for: chunk encryption
  |
  +-- Manifest Encryption Key (derived via HKDF-SHA256 from master key)
  |     Used for: encrypting file manifests (chunk lists, metadata)
  |
  +-- Name Encryption Key (derived via HKDF-SHA256 from master key)
        Used for: AES-SIV deterministic filename encryption
```

### Chunk Encryption

Each file chunk is independently encrypted:

```
Plaintext chunk
  --> zstd compress
  --> XChaCha20-Poly1305 encrypt
       Key:   file encryption key (256-bit)
       Nonce: 192-bit random (no nonce management needed)
       AAD:   chunk_index (8 bytes BE) || file_id (32 bytes)
  --> BLAKE3 hash of ciphertext (CAS key)
  --> Upload to SeaweedFS
```

Chunk format on disk/wire:
```
[24 bytes: nonce][N bytes: ciphertext][16 bytes: Poly1305 tag]
```

### Recovery Key

A BIP-39 24-word mnemonic is generated during `tcfs init`. This mnemonic can regenerate the master key independently of the passphrase. Store it offline.

## Credential Management

### Discovery Chain

Credentials are discovered in order of precedence:

1. `$CREDENTIALS_DIRECTORY/age-identity` -- systemd `LoadCredentialEncrypted`
2. `$SOPS_AGE_KEY_FILE` -- path to an age key file
3. `$SOPS_AGE_KEY` -- literal age key content
4. `~/.config/sops/age/keys.txt` -- default fallback

### SOPS-Encrypted Files

S3 credentials are stored in SOPS-encrypted YAML:

```yaml
access_key_id: ENC[AES256_GCM,data:...,iv:...,tag:...,type:str]
secret_access_key: ENC[AES256_GCM,data:...,iv:...,tag:...,type:str]
endpoint: http://dees-appu-bearts:8333
region: us-east-1
sops:
  age:
    - recipient: age1...
      enc: |
        -----BEGIN AGE ENCRYPTED FILE-----
        ...
        -----END AGE ENCRYPTED FILE-----
  mac: ENC[AES256_GCM,data:...,iv:...,tag:...,type:str]
  version: 3.8.1
```

### Memory Safety

- `secret_access_key` is stored as `secrecy::SecretString` (zeroized on drop)
- Temporary plaintext copies are explicitly `zeroize()`-d after conversion
- Debug formatting redacts secret fields: `S3Credentials { secret_access_key: "[REDACTED]" }`

## Credential Rotation

### Automated Rotation

tcfsd watches the SOPS credential file for changes. When the file is modified:

1. A 500ms debounce period coalesces rapid writes (e.g., atomic replace)
2. The file is re-decrypted with the configured age identity
3. The shared credential store is atomically swapped
4. Existing S3 connections continue with old credentials until the next request

### Manual Rotation

Use the `tcfs rotate-credentials` CLI command:

```bash
# Interactive -- prompts for new credentials
tcfs rotate-credentials

# Non-interactive -- reads from environment
AWS_ACCESS_KEY_ID=new-key \
AWS_SECRET_ACCESS_KEY=new-secret \
tcfs rotate-credentials --non-interactive
```

### Rotation Procedure

1. **Generate new credentials** on the S3/SeaweedFS admin console
2. **Run rotation**:
   ```bash
   tcfs rotate-credentials --cred-file /etc/tcfs/credentials.sops.yaml
   ```
3. **Verify**: tcfsd logs will show "credentials reloaded successfully"
4. **Deactivate old credentials** on the admin console
5. **Clean up old backups** (optional):
   ```bash
   ls /etc/tcfs/credentials.sops.yaml.bak.*
   ```

### Backup Policy

Each rotation creates a timestamped backup:
```
credentials.sops.yaml.bak.1740000000
```

These backups remain SOPS-encrypted and can be used to rollback if needed.

## TLS Configuration

### S3 (SeaweedFS)

```toml
[storage]
endpoint = "https://dees-appu-bearts:8333"
enforce_tls = true
ca_cert_path = "/etc/tcfs/certs/ca.pem"  # optional, for self-signed certs
```

When `enforce_tls = true`:
- HTTP endpoints produce an error at startup
- HTTPS endpoints are validated with system CA bundle (or custom CA if specified)

When `enforce_tls = false` (default):
- HTTP endpoints produce a warning in logs

### NATS

```toml
[sync]
nats_url = "tls://nats.example.com:4222"
nats_tls = true
nats_ca_cert = "/etc/tcfs/certs/nats-ca.pem"  # optional
```

## Device Identity

Each enrolled device has:
- An age X25519 keypair (generated during `tcfs init`)
- A device registry entry (`~/.local/share/tcfs/devices.json`)
- Platform keychain storage for session keys (macOS Keychain, GNOME Keyring, Windows Credential Manager)

### Revoking a Device

```bash
tcfs device revoke <device-name>
```

This marks the device as revoked in the registry. Future versions will implement key re-encryption to exclude revoked devices from new file keys.

## Config File Security

When `config_file_mode_check = true` (default), tcfsd warns at startup if the configuration file is world-readable.

Recommended permissions:
```bash
chmod 600 /etc/tcfs/config.toml
chmod 600 /etc/tcfs/credentials.sops.yaml
chown tcfs:tcfs /etc/tcfs/*
```

## Reporting Security Issues

Report security vulnerabilities to: jess@sulliwood.org

Please include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

We aim to acknowledge reports within 48 hours and provide a fix within 7 days for critical issues.
