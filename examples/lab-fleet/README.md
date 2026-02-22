# Lab Fleet Deployment

Example NixOS configuration fragments for deploying tcfs across 3 alpha machines.

## Machines

| Machine | Role | Conflict Mode | Git Sync |
|---------|------|---------------|----------|
| xoxd-bates | Primary workstation | auto | off |
| yoga | Hybrid server/workstation | interactive | bundle |
| petting-zoo-mini | Headless server | auto | off |

## Prerequisites

1. NATS server accessible at `nats://nats.tcfs.svc.cluster.local:4222`
2. SeaweedFS S3 endpoint at `dees-appu-bearts:8333`
3. tcfs packages built and available in your Nix store

## Usage

Import the relevant fragment into your machine's NixOS configuration:

```nix
# configuration.nix
{ ... }:
{
  imports = [
    ./hardware-configuration.nix
    /path/to/tummycrypt/examples/lab-fleet/yoga.nix
  ];
}
```

Or use the tcfs NixOS module directly with custom settings:

```nix
{ pkgs, ... }:
{
  imports = [ /path/to/tummycrypt/nix/modules/tcfs-daemon.nix ];

  services.tcfsd = {
    enable = true;
    package = pkgs.tcfsd;
    deviceName = "my-machine";
    conflictMode = "auto";
    natsUrl = "nats://localhost:4222";
  };
}
```

## Enrollment

After deploying, enroll each device:

```bash
tcfs device enroll --name $(hostname)
tcfs device list
```

## Verification

```bash
# Check daemon status
systemctl status tcfsd

# Push a test file
echo "hello fleet" > /tmp/test.txt
tcfs push /tmp/test.txt

# On another machine, pull it
tcfs pull tcfs/default/test.txt /tmp/test.txt
```
