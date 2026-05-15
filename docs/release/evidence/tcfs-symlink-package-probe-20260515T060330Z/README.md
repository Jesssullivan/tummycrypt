# TCFS Symlink Package Probe

Created: `2026-05-15T06:03:43Z`

This packet probes candidate `tcfs` binaries with `sync_symlinks = true`
against a tiny fixture containing `target.txt` and `link.txt -> target.txt`.

It is package/runtime drift evidence only. It does not claim production
readiness, Finder/FileProvider readiness, broad repo management, or home
directory takeover.

- Endpoint: `http://100.64.48.53:8333`
- Bucket: `tcfs`
- Prefix base: `tcfs-symlink-nix-consumer-probe-20260515T060330Z`
- Overall status: `passed`

Candidate results:

- `nix_current`: `preserved` (`tcfs 0.12.12`)

Files:

- `result.env`: machine-readable verdict, binary versions, and SHA-256s.
- `neo-nix-build.log` and `honey-nix-build.log`: Nix build commands that
  produced the producer and honey consumer store paths without installing either
  into a user profile.
- `neo-nix-tcfs-version.txt` and `honey-nix-tcfs-version.txt`: explicit
  version and SHA-256 proof for both binaries.
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
  --prefix-base tcfs-symlink-nix-consumer-probe-20260515T060330Z \
  --candidate nix_current=/nix/store/gp0cpr4vy5wijx53w45zf4xc6jb15nbi-tcfs-cli-0.12.12/bin/tcfs \
  --run-honey-mount \
  --honey-host honey \
  --honey-remote-dir /tmp/tcfs-symlink-nix-consumer-probe-20260515T060330Z \
  --honey-mount-root-base /tmp/tcfs-symlink-nix-consumer-probe-20260515T060330Z/mount \
  --honey-tcfs-bin /nix/store/yw9k9r34ma131ab3casy2h9129ds8qgb-tcfs-cli-0.12.12/bin/tcfs
```
