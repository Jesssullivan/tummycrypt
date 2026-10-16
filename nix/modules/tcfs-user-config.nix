# Pure renderer for programs.tcfs's config.toml attrset (nix/modules/tcfs-user.nix).
# Split out so nix/tests/tcfs-user-config-render.nix can evaluate it without
# Home Manager.
{ lib }:
cfg:
lib.recursiveUpdate {
  daemon.socket = "%t/tcfsd/tcfsd.sock";
  secrets.age_identity = cfg.identity;
  # Emit nats_tls explicitly — the Rust default is true, but lab NATS is
  # plaintext (tailnet-encrypted). Consumer `settings` can override.
  sync.nats_tls = cfg.natsTls;
} (lib.recursiveUpdate
  {
    # One `sync` set: a shallow `//` of two `sync` sets would drop sync_root.
    sync = lib.filterAttrs (_: v: v != null) {
      sync_root = cfg.syncRoot;
      nats_token_file = cfg.natsTokenFile;
    };
  }
  cfg.settings
)
