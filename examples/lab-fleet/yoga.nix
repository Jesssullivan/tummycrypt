# yoga fleet configuration fragment (Home Manager)
# Hybrid server/workstation — interactive conflicts, sync .git via bundle
{ pkgs, ... }:
{
  programs.tcfs = {
    enable = true;
    package = pkgs.tcfsd;
    identity = "~/.config/sops/age/keys.txt";

    deviceName = "yoga";
    conflictMode = "interactive";
    syncGitDirs = true;
    gitSyncMode = "bundle";
    natsUrl = "nats://nats.tcfs.tummycrypt.dev:4222";
    syncRoot = "~/tcfs";
    excludePatterns = [ "*.swp" "*.swo" ".direnv" "*.pyc" ];

    mounts = [
      { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "~/tcfs"; }
    ];
  };
}
