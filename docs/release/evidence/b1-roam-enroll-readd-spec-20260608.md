# B1 + roam-enroll re-add — ready-to-apply lab spec (2026-06-08)

Applies to `~/git/lab/nix/home-manager/tummycrypt.nix` + `nix/hosts/{macbook-neo,honey}.nix`.
Enables the repo-roam zero-diff canary (TIN-1620/G5 T13-Z) by restoring the roam-enroll
units and adding per-root raw-`.git` support. Daemon config is **never** modified.

## Step 0 — restore the removed roam-enroll infra
The removal commit (`tcfs: remove accidental roam-enroll from printstack skill landing`,
originally `ff577f3c`, re-hashed during your rebase) deleted 185 lines. Recover them:

```bash
cd ~/git/lab
# Find the current SHA of the removal commit (message is stable across rebases):
REMOVE=$(git log --oneline --all | grep "remove accidental roam-enroll" | head -1 | awk '{print $1}')
git revert --no-commit "$REMOVE"     # restores extraReconcileRoots option + reconcile units + host entries
# (equivalently: git checkout 70c31d5f -- nix/home-manager/tummycrypt.nix is NOT safe — other
#  edits have landed since; prefer the revert which is a targeted inverse-diff.)
```

## Step 1 — `tcfsReconcileScript` per-root config (tummycrypt.nix)
Replace the head of `tcfsReconcileScript = root:` (the `pkgs.writeShellScript ...` line) with a
`let`-bound per-root config, and make the `TCFS_CONFIG` export conditional:

```nix
  tcfsReconcileScript = root:
    let
      needsGitOverride = root.syncGitDirs || root.syncHiddenDirs || root.gitSyncMode != "bundle";
      # Secret-free per-root config: configTomlText carries NO secrets (master key is a
      # file PATH, S3 creds come from env), so a store-baked copy leaks nothing. Inject
      # the three [sync] git keys ONLY for this root; the daemon config is never touched.
      rootConfigFile = pkgs.writeText "tcfs-reconcile-${root.name}.toml"
        (builtins.replaceStrings [ "[sync]\n" ]
          [ "[sync]\nsync_git_dirs = ${bts root.syncGitDirs}\ngit_sync_mode = \"${root.gitSyncMode}\"\nsync_hidden_dirs = ${bts root.syncHiddenDirs}\n" ]
          configTomlText);
    in
    pkgs.writeShellScript "tcfsd-reconcile-${root.name}" ''
      set -u
      ...
      # was: export TCFS_CONFIG="${config.xdg.configHome}/tcfs/config.toml"
      export TCFS_CONFIG="${if needsGitOverride then rootConfigFile else "${config.xdg.configHome}/tcfs/config.toml"}"
      ...
```

## Step 2 — three submodule fields (after `assumeFreshPrefix` in the `extraReconcileRoots` submodule)
```nix
          syncGitDirs = mkOption {
            type = types.bool;
            default = false;
            description = "Set [sync].sync_git_dirs=true for THIS root only (recurse into .git). Renders a secret-free per-root config; the daemon config is untouched (no fleet-wide flip).";
          };
          gitSyncMode = mkOption {
            type = types.enum [ "bundle" "raw" ];
            default = "bundle";
            description = "[sync].git_sync_mode for this root. 'raw' syncs .git as ordinary files (.git-as-files dev-env roam).";
          };
          syncHiddenDirs = mkOption {
            type = types.bool;
            default = false;
            description = "Set [sync].sync_hidden_dirs=true for this root (allow repo dotfiles/dotdirs under the tree).";
          };
```

## Step 3 — host entries
`nix/hosts/macbook-neo.nix` (authoritative, first bulk push) — restore claude-projects AND add:
```nix
{ name = "git-roam-tool-daemon"; path = "/Users/jess/git/tinyland-tool-daemon";
  prefix = "git-roam/tool-daemon"; intervalSec = 300;
  assumeFreshPrefix = true;                 # FIRST empty-prefix bulk only; flip false after
  syncGitDirs = true; gitSyncMode = "raw"; syncHiddenDirs = true; }
```
`nix/hosts/honey.nix` (subordinate pull, SAME prefix = convergence):
```nix
{ name = "git-roam-tool-daemon"; path = "/home/jess/git/tinyland-tool-daemon";
  prefix = "git-roam/tool-daemon"; intervalSec = 300;
  assumeFreshPrefix = false;                # HEAD + PULL
  syncGitDirs = true; gitSyncMode = "raw"; syncHiddenDirs = true; }
```

## Step 4 — validate (no fleet mutation)
```bash
cd ~/git/lab
nix flake check 2>&1 | tail
# inspect the rendered per-root config is correct + secret-free:
nix eval --raw .#homeConfigurations.\"jess@macbook-neo\"...  # or build the agent and cat the .toml
# confirm the DAEMON config.toml is unchanged (defaults: sync_git_dirs=false).
```

## Step 5 — commit (only my files), flake bump, switch, canary
- `git add nix/home-manager/tummycrypt.nix nix/hosts/macbook-neo.nix nix/hosts/honey.nix` (NEVER `global-git-hooks.nix`)
- `git commit -S -m "tcfs: re-add roam-enroll units + per-root raw-.git config (B1); enroll tinyland-tool-daemon canary"`
- After tinyland #68 merges: `nix flake update tummycrypt` → commit flake.lock → `just nix-switch macbook-neo && just nix-switch honey` (disk dry-run first)
- Run R0–R5 from `docs/ops/repo-roam-test-plan-2026-06-08.md` against `tinyland-tool-daemon`.
