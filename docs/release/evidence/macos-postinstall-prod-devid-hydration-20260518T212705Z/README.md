# macOS Post-Install Production Dev ID FileProvider Hydration - 2026-05-18

Created: 2026-05-18T21:27:05Z

This packet archives the first green end-to-end run of the
`macos-postinstall-smoke.yml` workflow against the petting-zoo-mini-tcfs
self-hosted runner with the production Dev ID lane enabled. The notarized
`.pkg` was installed, the FileProvider extension registered, an index was
seeded over the tailnet, and the harness hydrated the expected file through
the installed package path with exact-content match.

## Result

- Workflow: `macos-postinstall-smoke.yml`
- Run: `26061402177`
- URL: `https://github.com/Jesssullivan/tummycrypt/actions/runs/26061402177`
- Runner label: `petting-zoo-mini`
- `fileprovider_testing_mode`: `false` (production Dev ID lane)
- Package source: notarization-proof run `26057944325`
  (artifact `dist-notarized-pkg-proof`), built from main commit `c08a0a4`
- Workflow source commit on smoke run: `0b1dc0c`
  (`macos-postinstall-smoke: derive enforce_tls from endpoint scheme`)
- Outcome: success

## What Was Proved

- The notarized `tcfs-0.12.12-macos-aarch64.pkg` installed cleanly on a
  self-hosted production-signed macOS host.
- `tcfsd` started under the package layout, loaded the master key, loaded S3
  credentials from `env:TCFS_S3_ACCESS`, and reported
  `storage: http://seaweedfs-tcfs:8333 [ok]`.
- A single seeded file was pushed by `tcfs push` to the run-scoped CAS prefix
  `gha/macos-postinstall/v0.12.12/26061402177-1` and a per-run device index
  was written.
- `tcfs index inspect` confirmed the index entry for the seeded file:

  ```json
  {
    "rel_path": "ci-smoke/0.12.12/postinstall-1.txt",
    "remote_prefix": "gha/macos-postinstall/v0.12.12/26061402177-1",
    "index_key": "gha/macos-postinstall/v0.12.12/26061402177-1/index/ci-smoke/0.12.12/postinstall-1.txt",
    "index_exists": true,
    "status": "visible",
    "entry_state": "committed",
    "visible_entry": {
      "manifest_hash": "8be0c98cf9acff679eff42df048ef3d7808658b40f9a1e87ee3ec09f99f7ba6b",
      "manifest_key": "gha/macos-postinstall/v0.12.12/26061402177-1/manifests/8be0c98cf9acff679eff42df048ef3d7808658b40f9a1e87ee3ec09f99f7ba6b",
      "manifest_exists": true,
      "size": 55,
      "chunks": 1,
      "kind": "regular_file",
      "symlink_target": null
    },
    "pending_entry": null
  }
  ```

- The installed `TCFSProvider.app` host app provisioned the master key into
  the shared Keychain, added the FileProvider domain, signalled the working
  set, and issued `requestDownload` for the seeded file:

  ```
  provisionConfig: added master key material to Keychain copy
  provisionConfig: provisioned 432 bytes to shared Keychain group
  add: OK - domain available
  signal workingSet: OK
  requestDownload: ci-smoke/0.12.12/postinstall-1.txt: OK nonce=tcfs-smoke-85003-26965-23900
  host app exiting
  ```

- The extension loaded its config from the shared Keychain (no embedded
  config required) — proving the post-install Keychain handshake works on
  Dev ID:

  ```
  TCFSFileProvider[...] loadConfig: no embedded config, trying Keychain
  TCFSFileProvider[...] loadConfig: loaded from shared Keychain
  ```

- The harness then read the hydrated file through the FileProvider path with
  byte-exact content match against the expected payload. The hydrated bytes
  archived as `harness/hydrated-expected-file` are:

  ```
  tcfs macOS post-install smoke v0.12.12 run 26061402177
  ```

- `harness/hydrate-read-error.log` is empty, i.e. the read returned cleanly.

## Chain of Fixes That Unblocked This Run

This was the first green run because four stacked production-only blockers
landed in order:

1. **Endpoint rotation Cloudflare -> tailnet.** The runner-side SeaweedFS
   endpoint was rotated off the Cloudflare hostname and onto the tailnet
   address `http://seaweedfs-tcfs:8333`, restoring the runner's ability to
   reach storage.
2. **`gh secret set` syntax fix.** The CI secret bootstrap helper was using
   a `gh secret set` invocation that silently truncated the endpoint into an
   empty value on the petting-zoo-mini runner; the corrected call writes the
   intended value.
3. **Workflow `enforce_tls` fix (commit `0b1dc0c`).** With the tailnet
   endpoint in place, `storage.enforce_tls = true` was tripping on an HTTP
   scheme. Commit `0b1dc0c` makes the workflow derive `enforce_tls` from the
   endpoint scheme so an `http://` tailnet endpoint no longer fails the
   daemon's TLS guard, while preserving the production WARN log line on the
   HTTP path.
4. **PZM stale per-user `TCFSProvider.app` cleanup.** The petting-zoo-mini
   runner host had a stale per-user copy of `TCFSProvider.app` that PlugInKit
   kept registering alongside the canonical `/Applications/TCFSProvider.app`,
   defeating strict installed preflight. Quarantining the stale bundle let
   the strict preflight resolve to a single canonical registration target.

## Status

- run_id: `26061402177`
- runner_label: `petting-zoo-mini`
- fileprovider_testing_mode: `false`
- notarized_pkg_source_run: `26057944325`
- notarized_pkg_source_commit: `c08a0a4`
- workflow_smoke_commit: `0b1dc0c`
- hydration_outcome: `exact-bytes-match`
- linear_issue: `TIN-133`
- github_issue: `#309`

## Key Artifacts (downloaded locally for verification)

- `harness/expected-file-index.json` — `tcfs index inspect` JSON for the
  seeded entry.
- `harness/hydrated-expected-file` — the bytes the FileProvider returned;
  match the expected payload exactly.
- `harness/hydrate-read-error.log` — empty.
- `harness/post-hydrate-status.log` — daemon status after hydration.
- `harness/host-request-launch.log` — host-app provisioning and
  `requestDownload` trace.
- `harness/extension-config.log` — FileProvider extension Keychain config
  load.
- `tcfsd.log`, `tcfs-push.log` — daemon and seeding push.
- `pluginkit.txt` — PlugInKit registration for
  `io.tinyland.tcfs.fileprovider(0.2.0)`.
- Codesign / spctl / syspolicy / entitlements dumps for both the host app
  and the extension are archived under the run artifact root.

## Claim Boundary

This packet proves one thing on the production Dev ID lane: a freshly
installed notarized package can hydrate a seeded file end-to-end and return
exact bytes through the installed FileProvider path on the
petting-zoo-mini-tcfs runner.

It does NOT prove:

- evict-and-rehydrate (round-trip materialize / dematerialize)
- in-place mutation reflected back through CAS
- conflict resolution from the Dev ID lane
- multi-file Finder lifecycle beyond the seeded entry
- long-running stability beyond one CI invocation

Those layered proofs remain open and are tracked separately under TIN-133
and the `next-workstream-queue` ops doc. Do not extend this packet's claim
beyond hydration without a follow-up archival run.
