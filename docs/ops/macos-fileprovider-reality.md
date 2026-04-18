# macOS Finder and FileProvider Reality

As of April 15, 2026, macOS is a real packaging and integration lane for tcfs,
but not yet a continuously proven release-grade desktop surface.

This document defines the actual workflow the repo supports today, separates
what is proven from what remains experimental, and records the highest-value
smoke path for the Finder/FileProvider surface.

## Supported Workflow In The Repo Today

The macOS FileProvider path currently consists of these pieces:

1. A packaged host app: `TCFSProvider.app`
2. A packaged non-UI FileProvider extension:
   `io.tinyland.tcfs.fileprovider`
3. A host-app registration step that removes and re-adds the
   `io.tinyland.tcfs` FileProvider domain on launch
4. A daemon and FileProvider socket path that the extension uses for
   enumeration, hydration, and watch signaling

In practical terms, the intended operator flow is:

1. install the macOS package or app bundle
2. ensure `tcfsd` is present and can start with the needed config
3. ensure the FileProvider extension is registered with `pluginkit`
4. launch `TCFSProvider.app` so the host app provisions config and re-adds the
   FileProvider domain
5. let `fileproviderd` enumerate the domain into `~/Library/CloudStorage/`
6. use Finder to enumerate and open items, which should hydrate on demand

## Proven Today

- CI proves the Rust staticlib, Swift sources, and macOS FileProvider build
  surfaces compile.
- Release automation builds `TCFSProvider.app`, packages it into the Apple
  Silicon `.pkg`, and runs `pluginkit -a` during package install.
- The host app does contain a real domain-registration path:
  it removes then re-adds `NSFileProviderDomainIdentifier("io.tinyland.tcfs")`
  on launch.
- The extension contains real enumeration, hydration, watch, and badge
  decoration code paths.

## Important Constraints

- The package postinstall script only auto-registers the extension if the app is
  installed at `/Applications/TCFSProvider.app`.
- The April 15, 2026 smoke path that used
  `installer -target CurrentUserHomeDirectory` landed the app at
  `~/Applications/TCFSProvider.app`, so that install path should be treated as
  requiring manual app launch and manual verification.
- The host app provisions config from `~/.config/tcfs/fileprovider/config.json`
  into Keychain as a best-effort startup step.

## Not Yet Proven

- A continuously exercised clean-host Finder/FileProvider acceptance lane from
  install through register, enumerate, hydrate, mutate, and conflict handling
- Finder badge visibility as a release gate
- Conflict UX and notification behavior as a release gate
- Release-day viability of every published macOS artifact without explicit
  post-cut smoke
- A stable claim that write flows are supported for end users on macOS

## Highest-Value Smoke Lane

This is the current best acceptance path for the macOS desktop surface.

### Preconditions

- a macOS machine with the packaged app and binaries installed
- a valid tcfs daemon config
- a valid FileProvider config at
  `~/.config/tcfs/fileprovider/config.json`
- a runnable `tcfsd`

### Named Harness

The repo now carries a named operator-facing harness for this lane:

```bash
bash scripts/macos-postinstall-smoke.sh \
  --expected-version "${VERSION}" \
  --config "$HOME/.config/tcfs/config.toml" \
  --expected-file "path/to/known/remote-backed-file"
```

Notes:

- `--expected-file` should point at a known remote-backed fixture relative to
  the `~/Library/CloudStorage/TCFS*` root for the current domain
- the helper assumes `tcfsd` is already runnable with a real config; it does
  not fabricate temp-home state or start a fake backend
- `#309` still tracks where this harness runs from a known-clean host per tag

### GitHub-Hosted Approximation

The repo now also carries a manual GitHub Actions executor for this lane:

- [`.github/workflows/macos-postinstall-smoke.yml`](../../.github/workflows/macos-postinstall-smoke.yml)

This is a `workflow_dispatch` lane on `macos-14` that:

- downloads the published `.pkg` for a tag
- runs `scripts/install-smoke.sh`
- writes a real tcfs config from repository secrets
- seeds a remote-backed fixture with `tcfs push`
- starts `tcfsd` with both primary and FileProvider sockets
- runs `scripts/macos-postinstall-smoke.sh`

Required repository secrets:

- `TCFS_S3_ENDPOINT`
- `TCFS_S3_BUCKET`
- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`
- `TCFS_NATS_URL`

Treat this as a clean-host approximation, not as already-proven release truth,
until at least one tagged run has passed and produced usable logs on GitHub.

### Manual Procedure

The script above codifies the manual steps below. Keep them here as the
operator-readable fallback and review path.

1. Verify the expected artifacts exist:

```bash
test -x /usr/local/bin/tcfsd || test -x "$HOME/usr/local/bin/tcfsd"
test -d /Applications/TCFSProvider.app || test -d "$HOME/Applications/TCFSProvider.app"
```

2. Verify the extension is registered with `pluginkit`:

```bash
pluginkit -m -A -D -i io.tinyland.tcfs.fileprovider
```

3. Launch the host app from the installed location:

```bash
open -a TCFSProvider
```

4. Verify the CloudStorage root appears:

```bash
ls "$HOME/Library/CloudStorage" | rg '^TCFS'
```

5. Verify enumeration by listing the mounted root:

```bash
find "$HOME/Library/CloudStorage" -maxdepth 2 -type f | head
```

6. Open or read a known remote-backed file and confirm that content hydration
   succeeds. This is the `--expected-file` target in the named harness.

7. Record whether badges or equivalent Finder state are visible, but treat that
   as observational evidence rather than a hard release gate.

### Pass Bar

Treat the current macOS desktop lane as materially proven only when all of the
following succeed on the same machine:

- extension registration is visible
- host app launch successfully re-adds the FileProvider domain
- a CloudStorage root appears
- enumeration works
- opening a placeholder-backed file hydrates content successfully
- `tcfsd` remains healthy throughout the path

## Relationship To Other Runbooks

- Use [Distribution Smoke Matrix](distribution-smoke-matrix.md) for packaged
  install proof.
- Use [`scripts/macos-postinstall-smoke.sh`](../../scripts/macos-postinstall-smoke.sh)
  for the named post-install harness that exercises this lane.
- Use [Apple Surface Status](apple-surface-status.md) for the broader Apple
  posture.
- Use this document for the macOS Finder/FileProvider desktop acceptance path
  itself.
