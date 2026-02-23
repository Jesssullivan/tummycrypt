# xoxd-bates fleet configuration fragment (Home Manager)
# Primary workstation — auto-resolve conflicts, no .git sync
{ pkgs, ... }:
{
  programs.tcfs = {
    enable = true;
    package = pkgs.tcfsd;
    identity = "~/.config/sops/age/keys.txt";

    deviceName = "xoxd-bates";
    conflictMode = "auto";
    syncGitDirs = false;
    natsUrl = "nats://nats-tcfs:4222";
    syncRoot = "~/tcfs";
    excludePatterns = [ "*.swp" "*.swo" ".direnv" ];

    mounts = [
      { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "~/tcfs"; }
    ];
  };
}
