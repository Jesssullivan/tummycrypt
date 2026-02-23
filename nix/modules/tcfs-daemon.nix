{ config, lib, pkgs, ... }:
# NixOS module: services.tcfsd
# Manages tcfsd as a systemd service with full hardening
#
# Example configuration:
#   services.tcfsd = {
#     enable = true;
#     configFile = "/etc/tcfs/config.toml";
#     credentialsFile = "/etc/tcfs/age-identity.txt";
#     mounts = [
#       { remote = "seaweedfs://dees-appu-bearts:8333/bucket"; local = "/mnt/tcfs"; }
#     ];
#   };
#
# With RemoteJuggler integration:
#   services.tcfsd = {
#     enable = true;
#     remoteJuggler = {
#       enable = true;
#       package = pkgs.remote-juggler;
#       identity = "github-personal";
#       kdbxPath = "/etc/tcfs/credentials.kdbx";
#     };
#   };

let
  cfg = config.services.tcfsd;
in {
  options.services.tcfsd = {
    enable = lib.mkEnableOption "tcfsd TummyCrypt filesystem daemon";

    package = lib.mkOption {
      type = lib.types.package;
      description = "tcfsd package to use";
    };

    configFile = lib.mkOption {
      type = lib.types.path;
      default = "/etc/tcfs/config.toml";
      description = "Path to tcfs.toml configuration file";
    };

    credentialsFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Path to age identity file for SOPS decryption";
    };

    mounts = lib.mkOption {
      type = lib.types.listOf (lib.types.submodule {
        options = {
          remote = lib.mkOption { type = lib.types.str; };
          local = lib.mkOption { type = lib.types.str; };
          readOnly = lib.mkOption { type = lib.types.bool; default = false; };
        };
      });
      default = [];
      description = "List of mounts to configure at startup";
    };

    # ── Fleet / multi-machine sync options ─────────────────────────────────
    deviceName = lib.mkOption {
      type = lib.types.str;
      default = config.networking.hostName;
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
      description = "NATS server URL for real-time state sync (e.g., nats://localhost:4222)";
    };

    syncRoot = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Local directory for auto-pull sync (daemon watches NATS and pulls files here)";
    };

    excludePatterns = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Glob patterns for files/dirs to exclude from sync";
    };

    remoteJuggler = {
      enable = lib.mkEnableOption "RemoteJuggler credential injection";

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.remote-juggler or null;
        description = "remote-juggler package for credential management";
      };

      identity = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "RemoteJuggler identity name for S3 credential resolution";
      };

      kdbxPath = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Path to KDBX database for credential lookup";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.tcfsd = {
      description = "TummyCrypt filesystem daemon (tcfsd)";
      after = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];

      path = lib.mkIf cfg.remoteJuggler.enable [ cfg.remoteJuggler.package ];

      serviceConfig = {
        Type = "notify";
        ExecStart = "${cfg.package}/bin/tcfsd --config ${cfg.configFile}";
        Restart = "on-failure";
        RestartSec = "5s";

        # Security hardening
        DynamicUser = true;
        PrivateTmp = true;
        PrivateDevices = false; # FUSE needs /dev/fuse
        DeviceAllow = [ "/dev/fuse rw" ];
        ProtectSystem = "strict";
        ProtectHome = false; # May need to write to user mounts
        ReadWritePaths = cfg.mounts;
        NoNewPrivileges = true;
        CapabilityBoundingSet = [ "CAP_SYS_ADMIN" ]; # FUSE mount
        AmbientCapabilities = [ "CAP_SYS_ADMIN" ];

        # Credentials
        LoadCredentialEncrypted = lib.mkIf (cfg.credentialsFile != null)
          "age-identity:${cfg.credentialsFile}";
      } // lib.optionalAttrs cfg.remoteJuggler.enable {
        # Inject credentials from RemoteJuggler KDBX before daemon starts
        ExecStartPre = let
          script = pkgs.writeShellScript "tcfsd-rj-creds" ''
            set -euo pipefail
            # Attempt to resolve S3 credentials from RemoteJuggler KDBX
            CREDS_DIR="''${CREDENTIALS_DIRECTORY:-/run/tcfsd/credentials}"
            mkdir -p "$CREDS_DIR"
            if command -v remote-juggler >/dev/null 2>&1; then
              remote-juggler kdbx get tcfs/s3-credentials \
                ${lib.optionalString (cfg.remoteJuggler.kdbxPath != null) "--database ${cfg.remoteJuggler.kdbxPath}"} \
                --format env > "$CREDS_DIR/s3-credentials" 2>/dev/null || true
            fi
          '';
        in "${script}";
      };

      environment = {
        TCFS_CONFIG = cfg.configFile;
        TCFS_DEVICE_NAME = cfg.deviceName;
        TCFS_CONFLICT_MODE = cfg.conflictMode;
        TCFS_SYNC_GIT_DIRS = lib.boolToString cfg.syncGitDirs;
        TCFS_GIT_SYNC_MODE = cfg.gitSyncMode;
      } // lib.optionalAttrs (cfg.natsUrl != null) {
        TCFS_NATS_URL = cfg.natsUrl;
      } // lib.optionalAttrs (cfg.excludePatterns != []) {
        TCFS_EXCLUDE_PATTERNS = lib.concatStringsSep "," cfg.excludePatterns;
      } // lib.optionalAttrs (cfg.credentialsFile != null) {
        CREDENTIALS_DIRECTORY = "%d";
      } // lib.optionalAttrs (cfg.remoteJuggler.enable && cfg.remoteJuggler.identity != "") {
        REMOTE_JUGGLER_IDENTITY = cfg.remoteJuggler.identity;
      } // lib.optionalAttrs (cfg.remoteJuggler.enable && cfg.remoteJuggler.kdbxPath != null) {
        TCFS_KDBX_PATH = toString cfg.remoteJuggler.kdbxPath;
      };
    };

    environment.systemPackages = [ cfg.package ];
  };
}
