# TIN-2856 Phase 0.4 — Fleet Credential Inventory

- **Mode**: STRICTLY READ-ONLY, METADATA-ONLY. No secret values were read, printed, or collected. Only paths, existence, octal modes, sizes (bytes), and mtimes.
- **Date collected**: 2026-07-16 (local neo time; honey/bumble timestamps in -0400)
- **Hosts**: neo (macOS, local), honey (Linux, ssh), bumble (Linux, ssh)
- **Runbook**: tummycrypt PR #557 rotation-ceremony, Phase 0.4

## Anomaly flags

| # | Severity | Anomaly |
|---|----------|---------|
| A1 | ~~HIGH~~ **FALSE POSITIVE** | `~/.config/tcfs/config.toml` "777" on honey/bumble was the **symlink's** mode (stat without -L). Verified with `stat -L` + `readlink -f`: both are home-manager store symlinks → `/nix/store/*-hm_tcfsconfig.toml`, target mode **444 read-only** (honey 1460 B, bumble 1459 B). Not writable by anyone. |
| A2 | ~~MED~~ **FALSE POSITIVE** | Same on neo: "755" was the symlink (`lrwxr-xr-x` → `/nix/store/*-home-manager-files/.config/tcfs/config.toml`); target mode **444**, 1555 B. |
| A3 | MED | TIN-1954 propagation STILL PRESENT: `dev.tinyland.prompt-pulse.plist` contains 2 matches for `TCFS_ENCRYPTION_KEY_FILE|encryption_passphrase` — env var `TCFS_ENCRYPTION_KEY_FILE` is set to the sops-nix `encryption_passphrase` path (path only; value not read). |
| A4 | INFO | neo Keychain: all three `tcfs` generic-password entries (`master-key`, `device-identity`, `session-token`) **absent** (security exit 44 = item not found). If the runbook expects Keychain-backed material on neo, this is a gap; if file-based is canonical, it is clean posture. |
| A5 | INFO | Dual tcfsd LaunchAgent plists on neo: root-owned `/Library/LaunchAgents/io.tinyland.tcfsd.plist` (644, 629 B, May 16) coexists with user `~/Library/LaunchAgents/dev.tinyland.tcfsd.plist`. Possible stale system-level agent. |
| — | PASS | Derivation path uniform fleet-wide: all 3 hosts use `master_key_file` only (wrapper SHA-256 path); **zero** `passphrase_file` or `kdf_salt` keys present anywhere. |
| — | PASS | All key-material files at expected modes/sizes: passphrase 0400/155 B, master.key 0600/32 B, github_token 0400/40 B on all hosts. tcfsd alive on all hosts. |

## neo (macOS, local)

| Item | Path | Exists | Mode | Size (B) | mtime | Notes |
|---|---|---|---|---|---|---|
| sops passphrase | `~/.config/sops-nix/secrets/tcfs/encryption_passphrase` | yes | 400 | 155 | 2026-07-15 22:31:30 | expected 0400 — OK |
| daemon master key | `~/.local/state/tcfsd/master.key` | yes | 600 | 32 | 2026-07-15 22:32:24 | expected 0600/32 B — OK |
| tcfs config | `~/.config/tcfs/config.toml` | yes | **755** | 87 | 2026-07-14 14:30:20 | **A2** wrong mode |
| fileprovider config | `~/.config/tcfs/fileprovider/config.json` | yes | 600 | 490 | 2026-07-14 14:30:36 | macOS-only, expected present |
| github token | `~/.config/sops-nix/secrets/api/github_token` | yes | 400 | 40 | 2026-07-15 22:31:30 | OK |

- config.toml keys present: `master_key_file = <redacted>` (only key; no `passphrase_file`, no `kdf_salt`) → **master_key_file path — compliant**
- Keychain (existence-only, no `-g`): `master-key` exit 44 (absent), `device-identity` exit 44 (absent), `session-token` exit 44 (absent) — **A4**
- LaunchAgents:
  - `/Library/LaunchAgents/`: `io.tinyland.tcfsd.plist` (rw-r--r--, root, 629 B, May 16) — **A5**
  - `~/Library/LaunchAgents/`: `dev.tinyland.prompt-pulse.plist`, `dev.tinyland.tcfs-mcp-reaper.plist`, `dev.tinyland.tcfsd-health.plist`, `dev.tinyland.tcfsd-reconcile-claude-projects.plist`, `dev.tinyland.tcfsd-reconcile-git-roam-tool-daemon.plist`, `dev.tinyland.tcfsd.plist`
  - `dev.tinyland.prompt-pulse.plist` (444, 8810 B, 2026-07-14 09:48:44): grep count for `TCFS_ENCRYPTION_KEY_FILE|encryption_passphrase` = **2** — **A3**. Redacted lines: `<key>TCFS_ENCRYPTION_KEY_FILE</key>` / `<string><path-redacted: sops-nix encryption_passphrase path></string>`
- `/etc/nix/nix.custom.conf`: `access-tokens` line count = **1** (readable without sudo)
- Daemon: `tcfsd` **alive**

## honey (Linux)

| Item | Path | Exists | Mode | Size (B) | mtime | Notes |
|---|---|---|---|---|---|---|
| sops passphrase | `~/.config/sops-nix/secrets/tcfs/encryption_passphrase` | yes | 400 | 155 | 2026-07-16 04:16:08 -0400 | OK |
| daemon master key | `~/.local/state/tcfsd/master.key` | yes | 600 | 32 | 2026-07-14 04:44:12 -0400 | OK |
| tcfs config | `~/.config/tcfs/config.toml` | yes | **777** | 87 | 2026-07-14 04:43:42 -0400 | **A1** world-writable |
| fileprovider config | `~/.config/tcfs/fileprovider/config.json` | **no** | — | — | — | absent as expected (Linux) |
| github token | `~/.config/sops-nix/secrets/api/github_token` | yes | 400 | 40 | 2026-07-16 04:16:08 -0400 | OK |

- config.toml keys present: `master_key_file = <redacted>` only → **master_key_file path — compliant**
- Daemon: `tcfsd` **alive**

## bumble (Linux)

| Item | Path | Exists | Mode | Size (B) | mtime | Notes |
|---|---|---|---|---|---|---|
| sops passphrase | `~/.config/sops-nix/secrets/tcfs/encryption_passphrase` | yes | 400 | 155 | 2026-07-14 04:48:58 -0400 | OK |
| daemon master key | `~/.local/state/tcfsd/master.key` | yes | 600 | 32 | 2026-07-14 04:48:59 -0400 | OK |
| tcfs config | `~/.config/tcfs/config.toml` | yes | **777** | 87 | 2026-07-14 04:48:48 -0400 | **A1** world-writable |
| fileprovider config | `~/.config/tcfs/fileprovider/config.json` | **no** | — | — | — | absent as expected (Linux) |
| github token | `~/.config/sops-nix/secrets/api/github_token` | yes | 400 | 40 | 2026-07-14 04:48:58 -0400 | OK |

- config.toml keys present: `master_key_file = <redacted>` only → **master_key_file path — compliant**
- Daemon: `tcfsd` **alive**
- Collection note: bumble's remote shell is fish; POSIX loop syntax fails over ssh — inventory used loop-free stat.
