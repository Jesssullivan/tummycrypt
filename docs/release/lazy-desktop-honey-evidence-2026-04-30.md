# Lazy Desktop-to-Honey Evidence - 2026-04-30

This note records the live proof for the TCFS lazy traversal demo goal: seed an
isolated Desktop-originated tree, mount that remote prefix on `honey`, traverse
clean names before hydration, and `cat` a remote-backed file successfully.

## Scope

- Local source fixture: `/Users/jess/Desktop/TCFS Demo`
- Honey mountpoint: `/home/jess/tcfs-demo/Desktop`
- Remote prefix: `seaweedfs://100.64.48.53:8333/tcfs/desktop-demo-jess-20260430T011603Z`
- Expected file: `Projects/tcfs-odrive-parity/honey-readme.txt`
- Evidence directory: `/tmp/tcfs-desktop-honey-evidence-20260430T011603Z`
- Honey binary: `/tmp/tummycrypt-desktop-demo-current/target/debug/tcfs`

The fixture is intentionally a contained Desktop subdirectory, not the whole
physical Desktop.

## Command Shape

```bash
TCFS_DESKTOP_DEMO_REMOTE=seaweedfs://100.64.48.53:8333/tcfs/desktop-demo-jess-20260430T011603Z \
TCFS_DESKTOP_DEMO_PUSH=1 \
TCFS_DESKTOP_DEMO_RUN_HONEY=1 \
TCFS_HONEY_START_MOUNT=1 \
TCFS_HONEY_FORWARD_AWS_ENV=1 \
TCFS_HONEY_TCFS_BIN=/tmp/tummycrypt-desktop-demo-current/target/debug/tcfs \
TCFS_DESKTOP_DEMO_EVIDENCE_DIR=/tmp/tcfs-desktop-honey-evidence-20260430T011603Z \
nix develop --accept-flake-config --command task lazy:desktop-honey-plan
```

`TCFS_HONEY_FORWARD_AWS_ENV=1` was used only because honey did not already have
its own credential source for this backend. The temporary remote env file was
removed by the helper, and the honey mount was unmounted after the smoke.

## Result

Push uploaded four files:

- `Notes/unsync-checklist.txt`
- `Photos/2026/april/manifest.txt`
- `Projects/tcfs-odrive-parity/Notes/product-goal.md`
- `Projects/tcfs-odrive-parity/honey-readme.txt`

The honey smoke passed:

```text
mounted root: /home/jess/tcfs-demo/Desktop
listing parent before cat: /home/jess/tcfs-demo/Desktop/Projects/tcfs-odrive-parity
cat hydrate target: Projects/tcfs-odrive-parity/honey-readme.txt
cat byte count: 115
lazy hydration mounted smoke passed
```

The mount log contained the expected daemon-auth fallback to direct FUSE mount
and no VFS index parse warnings after the directory-placeholder log hygiene fix.

## Caveats

- This proves the Linux/FUSE terminal surface, not Finder/FileProvider.
- The backend endpoint is tailnet/private and uses plaintext HTTP; it is valid
  for this lab proof but not production credential posture.
- Durable honey setup should install credentials on honey directly instead of
  forwarding local AWS environment variables for a long-lived mount process.
