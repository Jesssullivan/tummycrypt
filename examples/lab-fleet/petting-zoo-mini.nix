# petting-zoo-mini fleet configuration fragment
# Headless server — auto-resolve conflicts, defer on ambiguity
{ pkgs, ... }:
{
  services.tcfsd = {
    enable = true;
    package = pkgs.tcfsd;
    configFile = "/etc/tcfs/config.toml";

    deviceName = "petting-zoo-mini";
    conflictMode = "auto";
    syncGitDirs = false;
    natsUrl = "nats://nats.tcfs.svc.cluster.local:4222";
    excludePatterns = [ "*.swp" "*.swo" ".direnv" "*.log" ];

    mounts = [
      { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "/home/jsullivan2/tcfs"; }
    ];
  };
}
