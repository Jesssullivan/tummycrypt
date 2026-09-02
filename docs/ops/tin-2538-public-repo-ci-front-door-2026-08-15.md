# TIN-2538 public-repository CI front door

Date: 2026-08-15

## Decision

TCFS owns a small repository-local composite action for its public-read Nix
bootstrap. The action runs only after the exact TCFS revision is checked out
and verified, and only inside jobs assigned to literal sanctioned
`tinyland-nix` or `tinyland-dind` runner classes.

This action is independently authored TCFS source. It is not a copy, fork, or
public representation of any private GloriousFlywheel action. GloriousFlywheel
continues to own the runner, cache, and remote-execution substrate; TCFS owns
the public repository's invocation and credential boundary.

## Why the remote action reference was invalid

Natural PR runs at `f3484c1bd0b0b030eb8f29f5afa3754cb901f1b1` were claimed by
the sanctioned runner pools and then failed during action preparation with
`Unable to resolve action tinyland-inc/gloriousflywheel, not found`.
`Jesssullivan/tummycrypt` is a public user-owned repository, while the source
action repository is private and organization-owned. GitHub permits private
action sharing only to eligible private repositories under the documented
owner, organization, or enterprise boundary; it does not provide that private
action to this public cross-owner caller.

GitHub explicitly supports composite actions stored in the calling repository
and referenced by a relative path after checkout. That is the appropriate
public front door here:

- <https://docs.github.com/en/actions/tutorials/create-actions/create-a-composite-action#creating-a-composite-action-within-the-same-repository>
- <https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository#allowing-access-to-components-in-a-private-repository>

## Closed contract

The local action accepts only the existing audited command and six exact
public-read inputs. It fails closed unless all of these are true:

- GitHub reports a self-hosted runner environment.
- The site is one of the four named TCFS workload identities.
- `ATTIC_TOKEN` is empty and both cache-publication inputs are `false`.
- `NIX_USER_CONF_FILES` and `NETRC` are `/dev/null`.
- `NIX_CONFIG` adds only the public
  `https://nix-cache.tinyland.dev/main` substituter, its committed public
  verification key, and `netrc-file = /dev/null`.
- Every productive command queries the canonical effective `substituters` and
  `trusted-public-keys` settings and requires the exact endpoint and key.

The Nix manual documents `NIX_CONFIG` as inline configuration applied after
system and user configuration, and documents the public-key trust model for
binary caches:

- <https://nix.dev/manual/nix/latest/command-ref/conf-file#configuration-file>
- <https://nix.dev/guides/recipes/add-binary-cache.html>

The policy ledger binds the complete action source digest as well as every
protected workflow topology and command body. Regression tests independently
reject endpoint, public-key, netrc, token, publication, site, action-path, and
hosted-runner drift.

## Non-goals and holds

This source repair does not publish cache objects, supply credentials, dispatch
or rerun workflows, add hosted fallback, activate TCFS, or prove native Darwin
or Windows runtime behavior. TIN-3120 listener/registration and capability
binding remain separate. Durable exact-head CI must still be obtained through
natural runs before this draft can be considered for promotion.
