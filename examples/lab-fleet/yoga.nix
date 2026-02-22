# yoga fleet configuration fragment
# Hybrid server/workstation — interactive conflicts, sync .git via bundle
{ pkgs, ... }:
{
  services.tcfsd = {
    enable = true;
    package = pkgs.tcfsd;
    configFile = "/etc/tcfs/config.toml";

    deviceName = "yoga";
    conflictMode = "interactive";
    syncGitDirs = true;
    gitSyncMode = "bundle";
    natsUrl = "nats://nats.tcfs.svc.cluster.local:4222";
    excludePatterns = [ "*.swp" "*.swo" ".direnv" "*.pyc" ];

    mounts = [
      { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "/home/jsullivan2/tcfs"; }
    ];
  };
}
