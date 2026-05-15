# TCFS Symlink Package Probe

Created: `2026-05-15T05:11:39Z`

This packet probes candidate `tcfs` binaries with `sync_symlinks = true`
against a tiny fixture containing `target.txt` and `link.txt -> target.txt`.

It is package/runtime drift evidence only. It does not claim production
readiness, Finder/FileProvider readiness, broad repo management, or home
directory takeover.

- Endpoint: `http://100.64.48.53:8333`
- Bucket: `tcfs`
- Prefix base: `tcfs-symlink-mounted-probe-20260515T051126Z`
- Overall status: `passed`

Candidate results:

- `nix_current`: `preserved` (`tcfs 0.12.12`)

Files:

- `result.env`: machine-readable verdict, binary versions, and SHA-256s.
- `fixture.tsv`: fixture shape and expected symlink target.
- `symlink-targets.tsv`: mounted smoke symlink target fixture.
- `<label>.toml`: per-candidate config with `sync_symlinks = true`.
- `<label>.log`: per-candidate push output.

Mounted honey proof ran for `1` preserved candidate(s); failures: `0`.

The mounted proof starts tcfs mount on honey, checks clean-name
visibility, cats target.txt, and verifies link.txt -> target.txt.

Re-run command shape:

```bash
scripts/tcfs-symlink-package-probe.sh \
  --endpoint http://100.64.48.53:8333 \
  --bucket tcfs \
  --prefix-base tcfs-symlink-mounted-probe-20260515T051126Z \
  --candidate nix_current=/nix/store/v5snjr5v5ll0jwlgs42fn4a5r7f5rq8y-tcfs-cli-0.12.12/bin/tcfs \
  --run-honey-mount \
  --honey-host honey \
  --honey-remote-dir /tmp/tcfs-symlink-mounted-probe-20260515T051126Z \
  --honey-mount-root-base /tmp/tcfs-symlink-mounted-probe-20260515T051126Z/mount \
  --honey-tcfs-bin /tmp/tcfs-current-srcbin-a76d48db3e06/bin/tcfs
```
