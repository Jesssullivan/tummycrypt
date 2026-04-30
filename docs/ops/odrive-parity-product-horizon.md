# odrive Parity And Product Horizon

As of April 29, 2026, `tummycrypt` should treat odrive parity as a product
behavior target, not an implementation target.

The useful odrive lessons are visible user workflows:

- install a desktop agent and see a usable filesystem surface
- browse remote trees before downloading file contents
- hydrate on open
- unsync / free space safely
- choose folder-level sync behavior
- see status, progress, badges, and conflicts
- run the same operations from a scriptable CLI / headless agent
- sync or back up arbitrary local folders

The useful `tummycrypt` differentiation is the opposite: keep the architecture
more trustworthy than odrive by using modern encryption, content-addressed
chunks, vector clocks, open storage, and platform-native placeholders where the
platform has them.

## Public odrive Behavior To Match

Public odrive docs describe these relevant behaviors:

| odrive behavior | TCFS product implication |
| --- | --- |
| Desktop odrive folder with linked storage accounts as folders | TCFS needs a first-run desktop flow that creates or registers a visible user root. |
| Placeholder files/folders that expand on demand | Mounted TCFS and FileProvider should show clean names and hydrate on open. |
| Sync/unsync from desktop context menu and CLI | Every desktop action needs an equivalent CLI/headless-agent action. |
| Progressive/selective folder sync | TCFS needs folder policies for always, on-demand, never, threshold, and pinned behavior. |
| Auto-unsync and disk-space management | TCFS needs safe age/disk-pressure dehydration plus visible reclaimed-space feedback. |
| Sync any local folder | TCFS needs an explicit arbitrary-folder sync contract separate from mounted browsing. |
| Backup any local folder | Backup should be modeled separately from bidirectional sync: no accidental delete propagation, restore path, and version history. |
| Tray/status/progress/logs | TCFS needs user-visible health, active transfers, conflicts, and errors. |
| Headless sync agent / CLI | TCFS should keep scriptability as a first-class parity bar, especially for Linux and server hosts. |

Primary public odrive references:

- <https://docs.odrive.com/Features/sync/>
- <https://docs.odrive.com/User%20Manual/sync-your-odrive/>
- <https://docs.odrive.com/User%20Manual/sync-changes/>
- <https://docs.odrive.com/User%20Manual/manage-sync/>
- <https://docs.odrive.com/User%20Manual/manage-disk-space/>
- <https://docs.odrive.com/User%20Manual/backup-to-any-storage/>
- <https://docs.odrive.com/Features/web-client/>
- <https://docs.odrive.com/User%20Manual/odrive-sync-agent/>
- <https://docs.odrive.com/User%20Manual/odrive-sync-agent/odrive-cli/>

## Current TCFS Reality

TCFS already has much of the architectural substrate that odrive lacked or
implemented through older placeholder-file conventions:

- clean-name mounted VFS entries with on-open hydration
- shared index parsing for legacy `manifest_hash=...` records and versioned
  JSON entries written by the sync engine
- physical `.tc` / `.tcf` stubs for sync-root/offline representation
- macOS FileProvider code path with Finder placeholders / APFS dataless intent
- `FileSyncStatus` states: `not_synced`, `synced`, `active`, `locked`, `conflict`
- per-path locks for push, pull, hydrate, and unsync paths
- per-folder policy store with `always`, `on_demand`, and `never` modes
- auto-unsync config and runtime sweep with dirty-file checks
- centralized blacklist / exclusion semantics
- plan-then-execute reconciliation scaffolding

The important qualifier is proof. Most of those pieces are not yet proven as a
polished desktop product. The strongest current proof is still Linux CLI/daemon
and real-host backend sync. The weakest proof remains macOS Finder from package
install through register, enumerate, hydrate, mutate, conflict, and visible
status.

## Product Pillars

### 1. Lazy Cloud Files

Users should be able to browse a remote-backed tree without paying download
costs for file bodies.

Acceptance shape:

1. `ls` / Finder enumeration shows clean names before hydration.
2. `cat` / open hydrates exact content.
3. `unsync` / evict returns the item to a clean remote-backed state.
4. Re-open hydrates again.

Canonical proof docs:

- [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)

### 2. Sync Lifecycle Safety

Sync lifecycle behavior should be explicit and observable. The user should know
whether an item is not synced, synced, active, locked, or conflicted.

The next work is less about adding a first version of these concepts and more
about proving and surfacing them:

- expose status consistently through CLI, TUI, daemon RPC, and desktop UI
- prove per-path locking under concurrent operations
- prove dirty-child unsync safety recursively
- make conflict status and resolution visible in Finder and CLI

### 3. Folder Policy

odrive parity requires folder behavior, not just file operations.

Policy modes:

- `always`: keep content locally hydrated and push/pull automatically
- `on_demand`: enumerate remotely, hydrate on open, optionally auto-download
  files under a threshold
- `never`: ignore this subtree for sync and reconcile
- `pinned`: exempt from auto-unsync

Current TCFS has policy primitives; productionization needs CLI/desktop controls,
status reporting, and acceptance tests against watcher, reconcile, auto-unsync,
and mounted views.

### 4. Desktop Product Surface

macOS product maturity is not just "the package installs." The desktop bar is:

1. install the `.pkg`
2. provision config without hand-editing brittle paths
3. launch/register the FileProvider domain
4. see the TCFS root in Finder
5. enumerate remote items
6. open/hydrate content
7. mutate and resolve conflicts
8. see meaningful status/progress/badges/notifications
9. recover when daemon, storage, or credentials are wrong

Until that passes on clean hosts, macOS should remain labeled experimental.

### 5. Arbitrary Folder Sync And Backup

The proposed "make `~/Desktop` TCFS-managed" demo belongs to this pillar, not
to the mounted FileProvider proof by itself.

There are two distinct products here:

- **Sync any folder**: bidirectional sync of an existing local folder into a
  remote prefix. Deletes and conflicts matter.
- **Backup any folder**: one-way or version-preserving backup of an existing
  folder. Deletes should not casually destroy remote history.

TCFS should not blur those in docs or demos.

## Desktop Demo Contract

The high-drama demo goal is valuable, but the first version should not point at
the user's real `~/Desktop`.

### Unsafe First Demo

Avoid using the actual `~/Desktop` as the first managed sync root on a daily
driver.

Risks:

- Finder writes `.DS_Store` and metadata files.
- macOS may also have iCloud Desktop/Documents enabled.
- Screenshots, app drops, temporary files, and user data can enter the demo.
- A mistaken bidirectional delete or conflict path is too expensive.
- A physical sync-root demo can expose `.tc` / `.tcf` files, which is not the
  same UX as FileProvider placeholders.

### Safer Local Demo

Use an isolated local folder that still feels like a desktop:

```bash
mkdir -p "$HOME/Desktop/TCFS Demo"/{Projects,Photos,Notes}
```

The repo helper for this lane is:

```bash
task lazy:desktop-honey-plan
```

It is intentionally plan-only unless `TCFS_DESKTOP_DEMO_PUSH=1` or `--push` is
set, and it refuses the real `~/Desktop` by default. When honey already has the
required TCFS binary, mount permissions, and backend credentials, add
`TCFS_DESKTOP_DEMO_RUN_HONEY=1` and `TCFS_HONEY_START_MOUNT=1` to copy the smoke
artifacts to honey and run the remote check. If credentials are not already
installed on honey, `TCFS_HONEY_FORWARD_AWS_ENV=1` can forward the current AWS
environment through a temporary remote env file for that smoke only; unmount
after inspection when this mode starts the mount because the mount process
inherits those variables. If honey's installed `tcfs` is stale,
`TCFS_HONEY_TCFS_BIN=/path/to/tcfs` points the smoke at a current temporary
build.

Then configure `sync.sync_root` to that folder and use a disposable remote
prefix such as:

```text
desktop-demo/${USER}/${timestamp}
```

The demo can show:

1. nested local files pushed from the demo desktop folder
2. `tcfs unsync` converting selected hydrated files to physical stubs in that
   sync-root representation
3. CLI status before and after unsync
4. rehydrate through `tcfs hydrate`, daemon hydration, mounted VFS, or
   FileProvider depending on the surface being demonstrated

This proves arbitrary-folder sync and unsync without risking the user's real
Desktop.

### Cross-Host Honey Demo

Do not phrase the cross-host goal as "ssh into `honey:~/Desktop`" unless honey
is intentionally configured with the same remote prefix and a real TCFS surface
at that path.

The safer contract is:

1. macOS demo folder syncs to remote prefix `desktop-demo/...`
2. honey mounts the same prefix at an explicit disposable path, for example:

```text
~/tcfs-demo/Desktop
```

3. over SSH, run `find`, `ls`, and `cat` from that honey mount
4. verify `find` / `ls` do not hydrate content
5. verify `cat` hydrates and returns exact bytes
6. optionally clear/unsync the honey VFS cache and rehydrate

This demonstrates the broad odrive-like value: a desktop-originated tree is
available on another host as a lazy remote-backed filesystem. It does not
pretend that macOS Finder's Desktop folder and honey's home directory are the
same local filesystem.

### FileProvider Demo

For Finder parity, the proof target remains:

```text
~/Library/CloudStorage/TCFS*
```

not the physical `~/Desktop` folder.

The FileProvider demo should show clean Finder names, platform placeholders,
open-time hydration, and observable state. If the user wants a Desktop-like
Finder affordance, use a Finder sidebar favorite or alias to the TCFS
CloudStorage root rather than making `~/Desktop` the first FileProvider test.

## Productionization Backlog

Highest-value work from here:

1. Run and archive Linux lazy demo evidence on a FUSE-capable host.
2. Run and archive clean-host macOS Finder/FileProvider evidence.
3. Run and archive the dedicated arbitrary-folder sync demo using
   `~/Desktop/TCFS Demo` and honey.
4. Add CLI commands or docs for folder policy set/list/remove.
5. Surface auto-unsync results in CLI/TUI and desktop notifications.
6. Prove dirty-child unsync safety for directories in acceptance tests.
7. Prove status/progress/badges in Finder instead of treating them as comments
   in Swift code.
8. Split backup semantics from sync semantics before claiming odrive backup
   parity.
9. Add a diagnostic dump command that captures config redactions, daemon health,
   storage/NATS reachability, active transfers, recent errors, and FileProvider
   registration state.
