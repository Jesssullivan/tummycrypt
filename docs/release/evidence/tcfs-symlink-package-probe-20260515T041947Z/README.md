# TCFS Symlink Package Probe

Created: `2026-05-15T04:19:52Z`

This packet probes candidate `tcfs` binaries with `sync_symlinks = true`
against a tiny fixture containing `target.txt` and `link.txt -> target.txt`.

It is package/runtime drift evidence only. It does not claim production
readiness, Finder/FileProvider readiness, broad repo management, or home
directory takeover.

- Endpoint: `http://100.64.48.53:8333`
- Bucket: `tcfs`
- Prefix base: `tcfs-symlink-package-probe-20260515T041947Z`
- Overall status: `blocked`

Candidate results:

- `homebrew`: `skipped` (`tcfs 0.12.12`)
- `source_built`: `preserved` (`tcfs 0.12.12`)
- `nix_current`: `preserved` (`tcfs 0.12.12`)

Files:

- `result.env`: machine-readable verdict, binary versions, and SHA-256s.
- `fixture.tsv`: fixture shape and expected symlink target.
- `<label>.toml`: per-candidate config with `sync_symlinks = true`.
- `<label>.log`: per-candidate push output.

Re-run command shape:

```bash
scripts/tcfs-symlink-package-probe.sh \
  --endpoint http://100.64.48.53:8333 \
  --bucket tcfs \
  --prefix-base tcfs-symlink-package-probe-20260515T041947Z \
  --candidate homebrew=/opt/homebrew/opt/tcfs/bin/tcfs \
  --candidate source_built=/Users/jess/git/tummycrypt/target/codex-verify/debug/tcfs \
  --candidate nix_current=/nix/store/v5snjr5v5ll0jwlgs42fn4a5r7f5rq8y-tcfs-cli-0.12.12/bin/tcfs
```
