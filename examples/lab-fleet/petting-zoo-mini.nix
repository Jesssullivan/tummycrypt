# petting-zoo-mini fleet configuration fragment (Home Manager)
# Headless server — auto-resolve conflicts, defer on ambiguity
{ pkgs, ... }:
{
  programs.tcfs = {
    enable = true;
    package = pkgs.tcfsd;
    identity = "~/.config/sops/age/keys.txt";

    deviceName = "petting-zoo-mini";
    conflictMode = "auto";
    syncGitDirs = false;
    natsUrl = "nats://nats-tcfs:4222";
    syncRoot = "~/tcfs";
    excludePatterns = [ "*.swp" "*.swo" ".direnv" "*.log" ];

    mounts = [
      { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "~/tcfs"; }
    ];
  };
}
