# 2026-10-16 tcfs NATS token file (review follow-up)

Branch `feat/tcfs-nats-token-file-20261016` off `origin/main`. Rulings:
OI-1003-Q177 (TIN-4543, TIN-5136 answer 7), Q173, R-N13. No deployment,
host or cluster change was made.

## Review findings and outcomes

Two reviews of the first feature commit raised nine items. Each was checked
against the code before changing anything.

| # | Finding | Outcome |
|---|---------|---------|
| 1 | FIFO swapped in between stat and open hangs the reader | Fixed: `open_regular_file` opens `O_NONBLOCK \| O_NOFOLLOW \| O_CLOEXEC`; the handle's `fstat` (regular file, same dev/ino) is authoritative. Test `fifo_swapped_in_after_stat_does_not_block_open` hung (5 s timeout) before the fix. |
| 2 | `~/` with HOME unset/empty/`/` resolves to a root path | Fixed: new `HomeUnusable` error; token paths no longer use the shared `expand_tilde` fallback. |
| 3 | A token pasted into the path field is echoed | Fixed: `RelativePath` carries only the length; the info log runs after validation; redacted `config show` hides a non-absolute, non-`~/` value. |
| 4 | Owner not checked | Fixed: owner must be the effective uid or root (`NotOwned`). systemd credentials are root- or unit-owned, so they pass. |
| 5 | Token over plaintext `nats://` | No code change: `resolve_nats_url` already warns on every non-TLS connect. Documented in the ops section. |
| 6 | Home Manager `//` drops `sync_root` when `natsTokenFile` is set | Fixed: renderer moved to `nix/modules/tcfs-user-config.nix` with one `sync` set; `nix/tests/tcfs-user-config-render.nix` (wired as `checks.<system>.tcfs-user-config-render`) failed on the old expression. |
| 7 | No test proves the token reaches the server | Fixed: an in-process mock NATS server captures CONNECT `auth_token`. The reviewer's mutation (`token` replaced with `None`) now fails. |
| 8 | Mutation survivors in `nats_token.rs` | Fixed for 6 of 7; tests added for each. The size pre-check mutant is equivalent (the post-read cap gives the same `TooLarge`); the buffer capacity is now capped too. |
| 9 | Live fleet e2e connects without a token | Fixed: `fleet_live.rs` resolves `TCFS_NATS_TOKEN_FILE` through the same policy. |

## Not done

- The NixOS/HM modules still use `lib.types.enum'`, which current nixpkgs
  lacks. This is older than this branch.
- The HM module renders `secrets.age_identity = null` when `identity` is
  unset, and the TOML generator rejects null. This is also older than this
  branch.
- The `$CREDENTIALS_DIRECTORY` group-bit exception has not been tested on a
  live host.
