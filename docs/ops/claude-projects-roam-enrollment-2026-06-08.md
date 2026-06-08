# Roam Enrollment: `~/.claude/projects` via a Scheduled `tcfs reconcile` Unit

**Status:** spec / ready-to-apply (agent-drafted, needs operator review)
**Date:** 2026-06-08
**Track:** A (roam enrollment of a disjoint tree)
**Target hosts:** `macbook-neo` (Darwin), `honey` (Linux)
**Shared convergence prefix:** `agent/claude-projects`

---

## 0. Why this is a scheduled `reconcile` unit and not a `sync_root` change

TCFS has exactly **one** `sync_root` (rendered from `tinyland.tummycrypt.mount.path`,
`config.rs` / `tummycrypt.nix:171`). `~/.claude/projects` is a **disjoint tree**
(733 MB, append-only JSONL, 0 symlinks, no `.git`) that lives **outside** the TCFS
mount, so it cannot be folded into the daemon's single `sync_root` or its FileProvider
read path.

The enrollment vehicle is therefore the **CLI `reconcile` engine**, scheduled out of
band:

```
tcfs reconcile --path ~/.claude/projects --prefix agent/claude-projects --execute
```

This is the same diff/push/pull engine the daemon uses (`crates/tcfs-sync/src/reconcile.rs`),
with the **same fail-closed security deny-set** (`Blacklist::from_sync_config`, applied
in `collect_local_set`). The periodic reconcile is **NOT FileProvider-gated** — it only
needs S3 credentials and (for the unlocked master key) the running daemon. It runs even
on hosts where the FileProvider extension is disabled.

Key facts that shape the unit (all verified against `origin/main @ c7f725c`):

| Fact | Source | Consequence |
|---|---|---|
| `reconcile` flags are `--path`/`-p`, `--prefix`, `--execute`, `--state`/`TCFS_STATE_PATH` | `tcfs-cli/src/main.rs:275` | Unit invokes exactly these |
| Default is **dry-run**; `--execute` applies | `main.rs:282` | Unit MUST pass `--execute` |
| Crypto forces **serial** file upload (the parallel fast-path is gated when `encryption_present`) | `tcfs-sync/src/engine.rs:182` | First 733 MB push is ~10-30 min |
| `TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1` skips redundant remote HEADs on a known-empty prefix | `engine.rs:151-161` | Set it for the **first bulk** push only |
| Security deny-set is fail-closed and applied before the plan is built | `blacklist.rs:171`, `reconcile.rs:525` | Secrets never enter a push plan |
| `reconcile` is not FileProvider-gated | `main.rs:803` (no `fileProvider` guard) | Runs on neo (FP on) and honey (no FP) |

Because the reconcile uses the daemon's **master-wrapped** crypto context
(`build_encryption_context`, `WrapMode::Master` default), the bytes pushed to
`agent/claude-projects` are byte-identical in wrap shape to the existing `data`
prefix. **No wrap_mode change.** Convergence is proven by SHA256-after-decrypt on the
shared prefix.

`policy_file` / `sync-policy.toml` is a dead knob (zero `.rs` consumers) and is not
involved here. `reconcile_interval_secs` (config.rs default 300) is the daemon's own
loop, not this unit; this unit carries its own cadence via the launchd `StartInterval` /
systemd `OnUnitActiveSec`.

---

## 1. Concrete nix to paste into `nix/home-manager/tummycrypt.nix`

All three blocks below are paste-ready. They reuse the module's existing
`let`-block builders (`tcfsCliBin`, `socketPath`, `config.home.profileDirectory`,
the `*_FILE` sops-nix credential pattern from `daemonWrapper`/`mountWrapper`,
`config.xdg.stateHome`, `homeDir`). They introduce **one** new option
(`extraReconcileRoots`) and **one** new builder (`tcfsReconcileScript`).

### 1a. Option declaration

Add inside `options.tinyland.tummycrypt = { … }`, e.g. directly after the
`mount = { … };` block (around `tummycrypt.nix:557`). This mirrors the existing
`secrets.syncList` submodule-list style already in the module
(`tummycrypt.nix:895`).

```nix
    # =========================================================================
    # Extra Reconcile Roots (Track A — roam enrollment of disjoint trees)
    # =========================================================================
    # Schedules `tcfs reconcile --path <path> --prefix <prefix> --execute` for
    # trees that live OUTSIDE the single sync_root (e.g. ~/.claude/projects).
    # Each entry gets its own launchd agent (Darwin) / systemd oneshot+timer
    # (Linux), keyed by `name`. Uses the same fail-closed deny-set as the daemon.
    extraReconcileRoots = mkOption {
      type = types.listOf (types.submodule {
        options = {
          name = mkOption {
            type = types.str;
            description = ''
              Stable unit suffix (launchd Label / systemd unit name).
              Use a short slug, e.g. "claude-projects".
            '';
            example = "claude-projects";
          };
          path = mkOption {
            type = types.str;
            description = "Absolute local directory to reconcile (the disjoint tree).";
            example = "/Users/jess/.claude/projects";
          };
          prefix = mkOption {
            type = types.str;
            description = ''
              Shared remote prefix within the bucket. This is the convergence
              knob — every host using the SAME prefix converges on it.
            '';
            example = "agent/claude-projects";
          };
          intervalSec = mkOption {
            type = types.int;
            default = 300;
            description = "Seconds between reconcile cycles (cadence).";
          };
          assumeFreshPrefix = mkOption {
            type = types.bool;
            default = false;
            description = ''
              Export TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1 for the FIRST bulk push of
              a known-empty prefix (skips redundant remote HEADs). Flip back to
              false once the prefix has data, so deletes/overwrites stay correct.
            '';
          };
        };
      });
      default = [];
      description = ''
        Disjoint trees to enroll via a scheduled `tcfs reconcile --execute` unit.
        These are NOT the daemon sync_root and NOT FileProvider-gated; they run
        from the CLI with the same engine + fail-closed security deny-set.
      '';
      example = [
        { name = "claude-projects";
          path = "/Users/jess/.claude/projects";
          prefix = "agent/claude-projects";
          intervalSec = 300; }
      ];
    };
```

### 1b. The `tcfsReconcileScript` builder + per-root state isolation

Add to the top-level `let … in` block (e.g. directly after `mountWrapper`
ends, around `tummycrypt.nix:338`). It sources credentials **exactly** like
`daemonWrapper`/`mountWrapper` (hm-session-vars + `*_FILE` → `AWS_*`), guards on
the daemon socket, and isolates each root's sync state under
`$XDG_STATE_HOME/tcfsd/reconcile/<name>.json` so the disjoint tree never shares
state with the primary `sync_root`.

```nix
  # Build a credential-sourcing reconcile runner for one extraReconcileRoots entry.
  # Mirrors daemonWrapper/mountWrapper: source hm-session-vars, export AWS_* from
  # the sops-nix *_FILE paths, guard on the daemon socket, then exec the CLI.
  tcfsReconcileScript = root:
    pkgs.writeShellScript "tcfsd-reconcile-${root.name}" ''
      set -u

      # Source home-manager session vars (contains *_FILE env vars from sops-nix)
      HM_VARS="${config.home.profileDirectory}/etc/profile.d/hm-session-vars.sh"
      if [ -f "$HM_VARS" ]; then
        . "$HM_VARS"
      fi

      export TCFS_CONFIG="${config.xdg.configHome}/tcfs/config.toml"

      # Export S3 credentials from sops-nix *_FILE env vars (same as daemonWrapper)
      if [ -n "''${TCFS_S3_ACCESS_KEY_FILE:-}" ] && [ -f "''${TCFS_S3_ACCESS_KEY_FILE}" ]; then
        export AWS_ACCESS_KEY_ID="$(cat "$TCFS_S3_ACCESS_KEY_FILE")"
      fi
      if [ -n "''${TCFS_S3_SECRET_KEY_FILE:-}" ] && [ -f "''${TCFS_S3_SECRET_KEY_FILE}" ]; then
        export AWS_SECRET_ACCESS_KEY="$(cat "$TCFS_S3_SECRET_KEY_FILE")"
      fi

      # First-bulk-push helper: skip redundant remote HEADs on a fresh prefix.
      ${lib.optionalString root.assumeFreshPrefix ''export TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1''}

      # Guard: the daemon must be up so the master key / encryption session is
      # available. If the socket is absent, exit 0 (the next cycle retries).
      if [ ! -S "${socketPath}" ]; then
        echo "$(date -Iseconds) tcfsd-reconcile-${root.name}: daemon socket ${socketPath} absent, skipping"
        exit 0
      fi

      # Skip cleanly if the source tree does not exist yet on this host.
      if [ ! -d "${root.path}" ]; then
        echo "$(date -Iseconds) tcfsd-reconcile-${root.name}: ${root.path} not present, skipping"
        exit 0
      fi

      # Per-root state isolation: never share sync state with the primary sync_root.
      RECONCILE_STATE="${config.xdg.stateHome}/tcfsd/reconcile/${root.name}.json"
      mkdir -p "$(dirname "$RECONCILE_STATE")" 2>/dev/null || true

      echo "$(date -Iseconds) tcfsd-reconcile-${root.name}: reconciling ${root.path} -> ${root.prefix}"
      exec ${tcfsCliBin} reconcile \
        --path "${root.path}" \
        --prefix "${root.prefix}" \
        --state "$RECONCILE_STATE" \
        --execute
    '';
```

### 1c. The scheduled units — Darwin launchd agent + Linux systemd oneshot+timer

Add these two config blocks to the `mkMerge [ … ]` list in `config`
(e.g. directly after the existing Darwin/Linux health-check blocks, around
`tummycrypt.nix:1341`). They follow the module's established health/reaper unit
shape exactly: Darwin `launchd.agents.<label>` with `StartInterval` + log paths;
Linux `systemd.user.services` (oneshot) + `systemd.user.timers`
(`OnBootSec` + `OnUnitActiveSec`).

```nix
    # =========================================================================
    # Darwin: scheduled reconcile agents for extraReconcileRoots (Track A)
    # =========================================================================
    (mkIf (isDarwin && cfg.daemon.enable && cfg.extraReconcileRoots != []) {
      launchd.agents = lib.listToAttrs (map (root:
        lib.nameValuePair "tcfsd-reconcile-${root.name}" {
          enable = true;
          config = {
            Label = "dev.tinyland.tcfsd-reconcile-${root.name}";
            ProgramArguments = [ "${tcfsReconcileScript root}" ];
            StartInterval = root.intervalSec;
            RunAtLoad = false;
            ProcessType = "Background";
            LowPriorityIO = true;
            Nice = 10;
            StandardOutPath = "${homeDir}/.local/state/log/tcfsd-reconcile-${root.name}.log";
            StandardErrorPath = "${homeDir}/.local/state/log/tcfsd-reconcile-${root.name}.err";
          };
        }
      ) cfg.extraReconcileRoots);
    })

    # =========================================================================
    # Linux: scheduled reconcile oneshot + timer for extraReconcileRoots (Track A)
    # =========================================================================
    (mkIf (isLinux && cfg.daemon.enable && cfg.extraReconcileRoots != []) {
      systemd.user.services = lib.listToAttrs (map (root:
        lib.nameValuePair "tcfsd-reconcile-${root.name}" {
          Unit = {
            Description = "TummyCrypt scheduled reconcile (${root.name} -> ${root.prefix})";
            After = [ "tcfsd.service" ];
            Wants = [ "tcfsd.service" ];
          };
          Service = {
            Type = "oneshot";
            ExecStart = "${tcfsReconcileScript root}";
            Environment = [
              "PATH=/usr/bin:/bin:${lib.makeBinPath [ pkgs.coreutils ]}:${config.home.profileDirectory}/bin"
              "TCFS_CONFIG_DIR=${config.xdg.configHome}/tcfs"
            ];
          };
        }
      ) cfg.extraReconcileRoots);

      systemd.user.timers = lib.listToAttrs (map (root:
        lib.nameValuePair "tcfsd-reconcile-${root.name}" {
          Unit.Description = "TummyCrypt reconcile timer (${root.name})";
          Timer = {
            OnBootSec = "3m";
            OnUnitActiveSec = "${toString root.intervalSec}s";
            Persistent = true;
          };
          Install.WantedBy = [ "timers.target" ];
        }
      ) cfg.extraReconcileRoots);
    })
```

> Notes:
> - The Linux systemd `Service` writes its stdout/stderr to the journal
>   (`journalctl --user -u tcfsd-reconcile-claude-projects`), matching the
>   existing `tcfsd-health` / `tcfs-mcp-reaper` units which also rely on the
>   journal rather than explicit log files.
> - On Darwin, `RunAtLoad = false` keeps the first reconcile off the
>   login-storm path; the first cycle fires `StartInterval` seconds after load.

---

## 2. Per-host enablement snippets

### 2a. `nix/hosts/macbook-neo.nix`

Add near the other `tinyland.tummycrypt.*` lines (e.g. after the
`selectiveSync.dotfiles` block, ~line 58). neo already runs the daemon
(`tummycrypt.enable = true`, FileProvider on) and has the master key unlocked,
so the reconcile inherits the unlocked encryption session.

```nix
  # Track A: roam-enroll ~/.claude/projects onto the shared agent prefix.
  # Disjoint tree (733 MB append-only JSONL) reconciled out-of-band from the
  # single sync_root. BOUNDED-SUBSET FIRST: see step 3 before widening to the
  # full tree. Flip assumeFreshPrefix=true ONLY for the first bulk push.
  tinyland.tummycrypt.extraReconcileRoots = [
    {
      name = "claude-projects";
      path = "/Users/jess/.claude/projects";
      prefix = "agent/claude-projects";
      intervalSec = 300;          # ~5 min cadence
      assumeFreshPrefix = false;  # true ONLY for the very first bulk push
    }
  ];
```

### 2b. `nix/hosts/honey.nix`

Add near the other `tinyland.tummycrypt.*` lines (e.g. after
`secrets.enable = false;`, ~line 32). honey is Linux, runs the daemon, has no
FileProvider — the scheduled reconcile runs regardless. Path is the Linux home.

```nix
  # Track A: roam-enroll ~/.claude/projects onto the shared agent prefix.
  # Same convergence prefix as macbook-neo (agent/claude-projects) so the two
  # hosts converge. honey has no FileProvider; reconcile is not FP-gated.
  tinyland.tummycrypt.extraReconcileRoots = [
    {
      name = "claude-projects";
      path = "/home/jess/.claude/projects";
      prefix = "agent/claude-projects";
      intervalSec = 300;          # ~5 min cadence
      assumeFreshPrefix = false;  # true ONLY for the very first bulk push
    }
  ];
```

---

## 3. Bounded-subset first step, then widen

The first full 733 MB push is **serial** because crypto gates off the parallel
fast path (`engine.rs:182`), so a cold full push is ~10-30 min. De-risk by
proving the wiring on **one project subdir** before enrolling the whole tree.

1. **Bounded subset.** On BOTH hosts, temporarily point `path` at a single
   project subdir (pick a small one, same prefix on both hosts):

   ```nix
   # macbook-neo.nix  (temporary, bounded subset)
   { name = "claude-projects";
     path = "/Users/jess/.claude/projects/<one-small-project-dir>";
     prefix = "agent/claude-projects";
     intervalSec = 300;
     assumeFreshPrefix = true; }   # first push of a fresh prefix
   ```
   ```nix
   # honey.nix  (temporary, bounded subset — same prefix)
   { name = "claude-projects";
     path = "/home/jess/.claude/projects/<one-small-project-dir>";
     prefix = "agent/claude-projects";
     intervalSec = 300;
     assumeFreshPrefix = true; }
   ```

   Apply on neo first; let it push. Then apply on honey; it should **pull** what
   neo pushed. Prove neo<->honey convergence on the subset (see §4).

2. **First bulk push of the full tree.** Once the subset converges, widen `path`
   back to the full `~/.claude/projects` and keep `assumeFreshPrefix = true` for
   the **single** initial bulk cycle on the host that holds the authoritative
   copy. `TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1` only skips redundant remote HEADs on
   the known-empty prefix; per-file upload concurrency stays 1 under crypto, so
   budget ~10-30 min for the 733 MB push. Watch the unit log until `Done: N
   pushed`.

3. **Steady state.** Flip `assumeFreshPrefix = false` on **all** hosts (so
   subsequent cycles correctly HEAD the prefix and can reconcile deletes /
   overwrites). Keep `intervalSec = 300` (~5 min). The append-only JSONL shape
   means steady-state cycles are small incremental pushes.

> Reality check: ~733 MB, append-only, 0 symlinks, no `.git` — well-suited to a
> periodic reconcile. The deny-set still applies, so any stray secret-shaped file
> under the tree is fail-closed excluded (see §4).

---

## 4. Verification

Per cycle, the unit log (Darwin: `~/.local/state/log/tcfsd-reconcile-claude-projects.log`;
Linux: `journalctl --user -u tcfsd-reconcile-claude-projects`) shows the engine's
own lines (exact, from `tcfs-cli/src/main.rs`):

```
Reconciling /Users/jess/.claude/projects ↔ http://…:8333:agent/claude-projects/
Plan: 12 push, 0 pull, 3 create-dir, 0 delete-local, 0 delete-remote, 0 conflict, 41 up-to-date
Executing plan...
  → push  <project>/<session>.jsonl  (LocalNewer)
Done: 12 pushed, 0 pulled, 3 dirs-created, 0 deleted, 0 conflicts, 0 errors
```

**Checks:**

1. **Per-cycle plan lines.** Confirm `Plan: N push …` then `Done: N pushed …` on
   the authoritative host; on the other host confirm `N pull` then `N pulled`.
   When both hosts are converged: `Plan: 0 push, 0 pull, …` and
   `Nothing to do — local and remote are in sync.`

2. **Deny-set is fail-closed.** Any secret/credential/live-DB-shaped file under
   the tree never appears in a `→ push` line — it is filtered before the plan is
   built (`Blacklist::from_sync_config` → `collect_local_set`, `reconcile.rs:525`).
   Grep the unit log: no `auth.json`, `.credentials.json`, `*.sqlite`, `.env`
   should ever be in a push line. (For `~/.claude/projects` specifically: it is
   pure JSONL with no creds, but the deny-set is the safety net.)

3. **SHA256-after-decrypt convergence on the shared prefix.** Pick a file present
   on both hosts after a full cycle and compare plaintext hashes:

   ```sh
   # neo
   shasum -a 256 ~/.claude/projects/<proj>/<session>.jsonl
   # honey (after its reconcile pulls the same object)
   sha256sum /home/jess/.claude/projects/<proj>/<session>.jsonl
   ```

   Equal digests prove the master-wrapped round-trip (encrypt-on-push /
   decrypt-on-pull) preserved bytes across the shared `agent/claude-projects`
   prefix. Because wrap_mode stays `master` (default), the wrap shape is
   byte-identical to the existing `data` prefix.

4. **Daemon-socket guard.** If the daemon is down, the unit logs
   `daemon socket … absent, skipping` and exits 0 — no partial/failed push, the
   next cycle retries.

---

## 5. Operator-apply steps (explicit)

> These edits land in **`~/git/lab`** (NOT this repo). This document is the spec;
> the operator applies it.

1. **Edit `nix/home-manager/tummycrypt.nix`:**
   - Paste §1a (the `extraReconcileRoots` option) into
     `options.tinyland.tummycrypt`.
   - Paste §1b (the `tcfsReconcileScript` builder) into the top-level `let`.
   - Paste §1c (the Darwin launchd block + Linux systemd block) into the
     `config` `mkMerge` list.
2. **Edit `nix/hosts/macbook-neo.nix`:** paste §2a.
3. **Edit `nix/hosts/honey.nix`:** paste §2b.
4. **Bounded-subset first (§3 step 1):** set both hosts' `path` to one small
   project subdir and `assumeFreshPrefix = true`, then switch:
   ```sh
   just nix-switch macbook-neo
   just nix-switch honey
   ```
   Verify neo<->honey convergence on the subset (§4).
5. **Widen + first bulk push (§3 step 2):** set `path` back to the full
   `~/.claude/projects`, keep `assumeFreshPrefix = true` on the authoritative
   host for the single bulk cycle, switch that host, watch the unit log to
   `Done: N pushed`.
6. **Steady state (§3 step 3):** set `assumeFreshPrefix = false` on all hosts,
   `just nix-switch macbook-neo` and `just nix-switch honey`. Confirm ~5-min
   cycles report `Nothing to do` once converged.

**Rollback:** remove the `tinyland.tummycrypt.extraReconcileRoots` entry from the
host file and `just nix-switch <host>`; home-manager tears down the launchd agent
/ systemd timer. The remote `agent/claude-projects` prefix is independent of the
`data` prefix and can be left in place or pruned separately.
