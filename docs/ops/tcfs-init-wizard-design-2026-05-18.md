# `tcfs init` First-Run Wizard — Design Recon (TIN-1425)

**Status:** design recon for M11 sprint planning. Not an implementation plan.
**Date:** 2026-05-18.
**Scope:** propose the UX shape, surfaces, and integration points for a first-run
wizard that takes a freshly-installed tcfs binary to a green `tcfs status` with
zero file editing. Implementation is out of scope; per-device pubkey work is
TIN-1417 and is assumed to land first or in parallel.

## 1. Problem statement

After `installer -pkg TCFS-<version>.pkg -target /` on a fresh Mac:

- `scripts/macos-pkg-postinstall.sh:21-49` writes
  `/Library/LaunchAgents/io.tinyland.tcfsd.plist` and bootstraps the LaunchAgent
  into the console user's gui domain (`:60-62`).
- The plist `ProgramArguments` is hard-wired to
  `exec /usr/local/bin/tcfsd --config "$HOME/.config/tcfs/config.toml" --mode daemon`
  (`:36`).
- The postinstall script never creates `~/.config/tcfs/`, never writes a
  `config.toml`, and never prompts the user for storage credentials. The
  LaunchAgent therefore loops on missing-config errors. Logs go to
  `/tmp/tcfsd.stdout.log` and `/tmp/tcfsd.stderr.log` (`:40-43`) where no
  end-user will ever look.
- The bundled `TCFSProvider.app` host app (`swift/fileprovider/Sources/HostApp/HostApp.swift:194-204`)
  attempts to read `~/.config/tcfs/fileprovider/config.json`; when the file is
  absent it logs `provisionConfig: no config at ...` and silently skips Keychain
  provisioning. Domain registration still runs, so Finder shows an empty TCFS
  location that never enumerates.

Linux is similar: `crates/tcfsd/tcfsd.service:9` execs
`/usr/bin/tcfsd --config /etc/tcfs/config.toml`; the `.deb` does not seed that
file, so `systemctl start tcfsd` fails immediately and the user has no
discoverable next step.

Today's `tcfs init` (`crates/tcfs-cli/src/main.rs:2775-2890`) generates a BIP-39
mnemonic and writes `master.key`, but it does not write `config.toml`, does not
ask about storage, and ends with the operator-facing instruction
`Configure storage: tcfs config show` (`:2886`) — i.e., the user must still
hand-edit the config.

## 2. UX target

The user opens the menu-bar app (or runs `tcfs init` in a terminal) and is
walked, step by step, to a state where `tcfs status` prints `storage [ok]` and
the FileProvider domain can enumerate. No text-editor step. No credential
hunting in the README. No `master.key` chmod.

## 3. Wizard flow

Discrete, ordered steps. Each surface implements the same state machine; only
the rendering differs.

1. **Detect missing config.** If `~/.config/tcfs/config.toml` is absent OR
   `master.key` is absent, enter wizard. Otherwise, exit with a friendly
   "already initialized" message.
2. **Choose enrollment path** — branch:
   - **New identity** (default): generate fresh mnemonic + master key.
   - **Existing enrollment invite**: paste an invite blob (URL, base64, or
     QR-scanned token). The blob carries the storage endpoint + bucket + a
     short-lived bootstrap token. Skips steps 3 and 5; reuses fleet master key
     material via the invite-bound `tcfs device enroll --invite ...` path
     (TIN-1417 dependency).
3. **Choose storage backend** (new-identity path only):
   - SeaweedFS (single URL prompt; default `http://localhost:8333`)
   - Generic S3-compatible (endpoint + region + bucket + access/secret pair)
   - Skip-for-now (write config with `[storage] backend = "none"`, mark the
     domain disabled so the LaunchAgent stops restart-looping)
4. **Generate BIP-39 mnemonic.** Reuse `tcfs_crypto::generate_mnemonic()`
   (already called at `crates/tcfs-cli/src/main.rs:2826`). Display the 24 words
   once. Require an explicit "I wrote it down" confirmation (CLI: type `yes`;
   TUI: typed confirmation; macOS app: a gated modal with a checkbox the user
   must tick before the "Continue" button enables).
5. **Write `master.key`.** Raw 32 bytes, mode `0o600`, owned by the invoking
   user (mirrors `:2855-2867`).
6. **Write `config.toml`.** Render from a template populated by steps 2–3.
   Include `[crypto].master_key_file`, `[storage]` block, `[sync]` defaults,
   `[grpc].socket_path`.
7. **Verify** by spawning `tcfs status` (or calling the equivalent in-process
   function); only declare success when `storage [ok]` is observed. Surface the
   exact failing line if not.
8. **Hand-off.** Print/show "Open TCFS in Finder" (macOS) or
   `systemctl --user start tcfsd` (Linux) plus a link to the recovery doc.

## 3.1 2026-05-19 Decision Log

These calls resolve the open questions for the first implementation slice:

1. **Split fresh setup from fleet join.** `tcfs init` owns new-local identity
   and config creation. `tcfs enroll <invite>` owns joining an existing fleet.
   The GUI may present both choices, but the command surface should stay
   explicit so users do not paste an invite into a flow that generates a new
   root.
2. **Do not fall back to placeholder device keys.** Fresh `tcfs init` can ship
   for single-operator setup, but invite/join verification must stay blocked
   until TIN-1417/TIN-1424 provide real per-device public keys, full invite
   payload signatures, and no raw long-lived storage secrets in invites.
   2026-05-25 update: the local `tcfs init` and `tcfs device enroll` paths now
   generate real age/X25519 device keys; invite/join remains blocked on the
   rest of TIN-1417/TIN-1424.
3. **Use an inline macOS sheet, backed by Rust-owned state.** Do not shell out
   to Terminal for the primary Mac path. The Swift host app should render the
   form, but validators, config rendering, and final verification stay in the
   Rust CLI/library path via a machine-readable contract.
4. **Require mnemonic confirmation.** GUI/TUI flows should retype four random
   words before writing `master.key`. CLI can keep a typed confirmation for
   interactive use and reserve non-interactive bypasses for explicit flags.
5. **Hide passphrase mode from the default wizard.** Keep the existing
   Argon2id/password path as a power-user CLI flag until recovery, support, and
   lost-password UX are documented.
6. **Do not offer "skip storage" in the default GUI path.** A half-configured
   install looks too much like success. Keep an explicit advanced
   `tcfs init --skip-storage` escape hatch only if it writes a disabled config,
   exits the daemon cleanly, and makes `tcfs status` say storage is not
   configured rather than failing mysteriously.

Implementation order follows from those calls: extend the CLI state machine
first, wire the macOS sheet second, and treat TUI as a follow-up unless it
falls out cheaply from the shared Rust state machine.

## 4. Three surfaces

| Surface | Renderer | Trigger | Shared logic |
|---|---|---|---|
| `tcfs init` CLI | extended `cmd_init` in `crates/tcfs-cli/src/main.rs` | user runs it; postinstall scripts nudge toward it | wizard state machine in a new `tcfs-cli` module, e.g. `cli::init::wizard` |
| `tcfs-tui` init view | new tab/modal in `crates/tcfs-tui/src/ui` | TUI launched with no config | same module |
| macOS host app first-run modal | SwiftUI sheet in `TCFSProviderApp` | host app launched and `~/.config/tcfs/config.toml` missing | calls `tcfs init --machine` over a JSON stdio contract (or invokes the new module via the existing UniFFI/cbindgen surface in `tcfs-file-provider`) |

The state machine, prompts, validators, and `config.toml` template **must** be
defined once in Rust and called by all three surfaces. The Swift app should not
own its own copy of the prompt sequence.

## 5. Integration points

- **macOS pkg postinstall** (`scripts/macos-pkg-postinstall.sh`): after
  bootstrap, detect missing `${HOME}/.config/tcfs/config.toml` for the console
  user; post a user-notification via `osascript -e 'display notification ...'`
  or open the host app with a `--first-run` flag so the wizard modal fires.
  The LaunchAgent plist should also learn a "no config → exit 0, do not loop"
  guard so it stops thrashing while the user is still in the wizard.
- **Linux systemd unit** (`crates/tcfsd/tcfsd.service`): on missing
  `/etc/tcfs/config.toml` or per-user `~/.config/tcfs/config.toml`, exit with
  a clear journald message: `tcfsd: no config; run 'tcfs init' to set up`.
  Optionally ship a `tcfsd.service` `ExecStartPre=/usr/bin/tcfs init --check`
  hook that fails fast with that message.
  - 2026-05-24 implementation note: the daemon binary now fails missing config
    with a `tcfs init --config-out ...` recovery hint, and
    `scripts/install-smoke.sh` runs installed `tcfs init` before daemon startup
    when the installed CLI supports `--config-out`. Older published packages
    and daemon-only surfaces fall back to a minimal explicit smoke config.
    systemd/LaunchAgent wiring is still a follow-on.
  - 2026-05-25 evidence note: PR #450 landed this behavior with local
    validation through `scripts/test-install-smoke.sh`,
    `scripts/install-smoke.sh --tcfs target/debug/tcfs --tcfsd target/debug/tcfsd`,
    `cargo test -p tcfsd`, and `cargo test -p tcfs-cli init`. This does not
    close storage-backed first-use or the macOS inline wizard.
- **macOS host app** (`swift/fileprovider/Sources/HostApp/HostApp.swift`):
  when `provisionConfig()` (`:194-204`) finds no config, instead of silently
  logging and continuing, launch the first-run wizard inline (sheet) before
  calling `NSFileProviderManager.add(domain)`.

## 6. Acceptance criteria

- **Fresh-Mac smoke:** install the signed `.pkg` on a clean macOS host, click
  the menu-bar icon (or accept the postinstall notification), complete the
  wizard, then prove FileProvider enumerate + hydrate of one file. Zero file
  editing. Captured as a lane in `docs/ops/distribution-smoke-matrix.md`.
- **Fresh-Linux smoke:** install the `.deb`, run `tcfs init` (only command the
  README mentions for first-run), then `systemctl --user start tcfsd` succeeds
  and `tcfs status` reports `storage [ok]`. No editor invoked.
- **Idempotency:** running the wizard twice on a configured host refuses with
  the existing `Already initialized` message (`crates/tcfs-cli/src/main.rs:2793-2798`).
- **No regression** of the headless `tcfs init --non-interactive --password ...`
  contract used by integration tests at
  `crates/tcfs-cli/tests/cli_parsing_test.rs`.

## 7. Touch points

- `crates/tcfs-cli/src/main.rs::cmd_init` (lines `2775-2890`) — extend.
- `crates/tcfs-cli/src/` — new `init/wizard.rs` module hosting the shared state
  machine.
- `crates/tcfs-tui/src/ui` and `crates/tcfs-tui/src/app.rs` — new first-run
  modal; today there is no `Init` tab (see `Tab::ALL` at
  `crates/tcfs-tui/src/app.rs:19-25`).
- `swift/fileprovider/Sources/HostApp/HostApp.swift` (`provisionConfig`,
  lines `194-251`) — wire the wizard before domain add.
- `scripts/macos-pkg-postinstall.sh` (lines `21-64`) — notification + plist
  no-config guard.
- `crates/tcfsd/tcfsd.service` (line `9`) — journald message or
  `ExecStartPre=tcfs init --check`.

## 8. Open questions for user review

1. **One command or two?** Should `tcfs init` cover both "new identity" and
   "join existing fleet via invite", or do we split into `tcfs init` (new) and
   `tcfs enroll <invite>` (join)? The latter is easier to document and harder
   to misuse, but doubles the surface the postinstall hook has to nudge toward.
2. **TIN-1417 ordering.** Local real device keys have landed for fresh setup.
   The remaining ordering question is invite/join verification: keep that path
   blocked until per-device wrapping and pairing/admin gates are in place.
3. **macOS host-app inline vs. terminal hand-off.** Should the first-run sheet
   in `TCFSProviderApp` render the prompts itself (SwiftUI form, full control
   over UX) or shell out to `Terminal.app` running `tcfs init` (one source of
   truth, but breaks the no-terminal target for non-technical Mac users)?
4. **Mnemonic display safety.** Plain on-screen render with a "wrote it down"
   checkbox, or a gated modal that requires the user to retype 4 random words
   before continuing? The latter is what hardware wallets do; the former is
   what the current CLI does. Where do we want to land for a desktop product
   that may be used over screen-share?
5. **Passphrase escrow.** Should the wizard offer the existing
   `--password`/Argon2id path (`crates/tcfs-cli/src/main.rs:2801-2809`) as a
   user-visible option ("I want a passphrase instead of a recovery phrase"),
   or treat it as a power-user-only flag and hide it from the wizard UI?
6. **"Skip storage for now."** Is the disabled-domain escape hatch (step 3,
   option c) worth shipping, or does it just create a population of installs
   stuck in a half-configured state we cannot debug remotely?
