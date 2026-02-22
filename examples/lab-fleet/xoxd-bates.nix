# xoxd-bates fleet configuration fragment
# Primary workstation — auto-resolve conflicts, no .git sync
{ pkgs, ... }:
{
  services.tcfsd = {
    enable = true;
    package = pkgs.tcfsd;
    configFile = "/etc/tcfs/config.toml";

    deviceName = "xoxd-bates";
    conflictMode = "auto";
    syncGitDirs = false;
    natsUrl = "nats://nats.tcfs.svc.cluster.local:4222";
    excludePatterns = [ "*.swp" "*.swo" ".direnv" ];

    mounts = [
      { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "/home/jsullivan2/tcfs"; }
    ];
  };
}
