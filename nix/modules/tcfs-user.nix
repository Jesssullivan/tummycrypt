{ config, lib, pkgs, ... }:
# Home Manager module: programs.tcfs
# Manages per-user tcfs CLI config, user systemd unit, and shell completions
#
# Example:
#   programs.tcfs = {
#     enable = true;
#     identity = "~/.config/sops/age/keys.txt";
#     mounts = [
#       { remote = "seaweedfs://host/bucket"; local = "~/tcfs"; }
#     ];
#   };
#
# With RemoteJuggler integration:
#   programs.tcfs = {
#     enable = true;
#     remoteJuggler = {
#       enable = true;
#       identity = "github-personal";
#     };
#   };

let
  cfg = config.programs.tcfs;
  toml = pkgs.formats.toml {};
in {
  options.programs.tcfs = {
    enable = lib.mkEnableOption "tcfs TummyCrypt filesystem client";

    package = lib.mkOption {
      type = lib.types.package;
      description = "tcfs package";
    };

    identity = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Path to age identity file";
    };

    mounts = lib.mkOption {
      type = lib.types.listOf (lib.types.submodule {
        options = {
          remote = lib.mkOption { type = lib.types.str; };
          local = lib.mkOption { type = lib.types.str; };
        };
      });
      default = [];
    };

    settings = lib.mkOption {
      type = lib.types.attrs;
      default = {};
      description = "Additional tcfs.toml settings";
    };

    # ── Fleet / multi-machine sync options ─────────────────────────────────
    deviceName = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Device name for fleet identification (defaults to hostname)";
    };

    conflictMode = lib.mkOption {
      type = lib.types.enum' [ "auto" "interactive" "defer" ];
      default = "auto";
      description = "Conflict resolution mode: auto (deterministic), interactive (prompt), defer (skip)";
    };

    syncGitDirs = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to sync .git directories";
    };

    gitSyncMode = lib.mkOption {
      type = lib.types.enum' [ "bundle" "raw" ];
      default = "bundle";
      description = "Git sync mode: bundle (git bundle) or raw (file-by-file)";
    };

    natsUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "NATS server URL for real-time state sync";
    };

    excludePatterns = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Glob patterns for files/dirs to exclude from sync";
    };

    remoteJuggler = {
      enable = lib.mkEnableOption "RemoteJuggler integration for credential management";

      identity = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "RemoteJuggler identity name (e.g., 'github-personal')";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."tcfs/config.toml".source = toml.generate "tcfs-config" (
      lib.recursiveUpdate {
        daemon.socket = "%t/tcfsd/tcfsd.sock";
        secrets.age_identity = cfg.identity;
      } cfg.settings
    );

    # User systemd unit for tcfsd
    systemd.user.services.tcfsd = {
      Unit = {
        Description = "TummyCrypt filesystem daemon (user)";
        After = [ "network.target" ];
      };
      Service = {
        Type = "notify";
        ExecStart = "${cfg.package}/bin/tcfsd --mode daemon";
        Restart = "on-failure";
        Environment = lib.mkMerge [
          [ "TCFS_CONFLICT_MODE=${cfg.conflictMode}" ]
          [ "TCFS_SYNC_GIT_DIRS=${lib.boolToString cfg.syncGitDirs}" ]
          [ "TCFS_GIT_SYNC_MODE=${cfg.gitSyncMode}" ]
          (lib.mkIf (cfg.deviceName != null) [
            "TCFS_DEVICE_NAME=${cfg.deviceName}"
          ])
          (lib.mkIf (cfg.natsUrl != null) [
            "TCFS_NATS_URL=${cfg.natsUrl}"
          ])
          (lib.mkIf (cfg.excludePatterns != []) [
            "TCFS_EXCLUDE_PATTERNS=${lib.concatStringsSep "," cfg.excludePatterns}"
          ])
          (lib.mkIf (cfg.remoteJuggler.enable && cfg.remoteJuggler.identity != null) [
            "REMOTE_JUGGLER_IDENTITY=${cfg.remoteJuggler.identity}"
          ])
        ];
      };
      Install = {
        WantedBy = [ "default.target" ];
      };
    };
  };
}
