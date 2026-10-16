# Eval test for the programs.tcfs config.toml renderer.
# Run: nix-instantiate --eval --strict nix/tests/tcfs-user-config-render.nix \
#        --arg lib '(import <nixpkgs> {}).lib'
# (flake.nix wires it into `checks` with the pinned nixpkgs lib.)
{ lib }:
let
  render = import ../modules/tcfs-user-config.nix { inherit lib; };
  base = {
    identity = "~/.config/sops/age/keys.txt";
    natsTls = false;
    syncRoot = null;
    natsTokenFile = null;
    settings = { };
  };
  both = render (base // {
    syncRoot = "~/tcfs";
    natsTokenFile = "/run/user-secrets/tcfs-nats-token";
  });
  withSettings = render (base // {
    syncRoot = "~/tcfs";
    natsTokenFile = "/run/user-secrets/tcfs-nats-token";
    settings.sync.workers = 4;
  });
  neither = render base;
  check = name: cond: if cond then true else throw "tcfs-user-config-render: ${name}";
in
lib.all lib.id [
  # Regression: a shallow `//` merge made the second `sync` set replace the
  # first, so setting natsTokenFile dropped sync_root.
  (check "syncRoot kept when natsTokenFile is set" (both.sync.sync_root or null == "~/tcfs"))
  (check "natsTokenFile rendered" (both.sync.nats_token_file or null == "/run/user-secrets/tcfs-nats-token"))
  (check "nats_tls kept" (both.sync.nats_tls == false))
  (check "settings merge deep" (withSettings.sync.workers == 4 && withSettings.sync.sync_root == "~/tcfs"
    && withSettings.sync.nats_token_file == "/run/user-secrets/tcfs-nats-token"))
  (check "no token key without natsTokenFile" (!(neither.sync ? nats_token_file)))
  (check "never renders an inline token" (!(both.sync ? nats_token)))
]
