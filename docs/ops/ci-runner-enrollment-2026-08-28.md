# CI runner enrollment: `tummycrypt-*` is a scale-set anchor, not a `runs-on` label

Date: 2026-08-28. Measured on neo against `origin/main` c782bb9, ci-templates
v3.1.0 (`d8d178c022a0f84853d53a2c8fe0fc90115f0949`), GloriousFlywheel `main`,
and `Jesssullivan/jesssullivan-infra` `HEAD`.

Lane 4 of the release-flow migration was specified as "`release.yml` `runs-on`
→ `tummycrypt-*` class". Taken literally that is wrong and would strand every
job. Taken as intended it is right, already provisioned, and partly executed by
this PR. This note records the difference, because the first two hours of the
investigation went to the wrong conclusion and the receipts are worth keeping.

## 1. `tummycrypt-*` cannot be a `runs-on` label

`GloriousFlywheel scripts/validate-arc-runner-taxonomy.py` — the authority for
tinyland tfvars `runner_label` values — lists `tummycrypt` explicitly in
`PROJECT_IDENTITY_TOKENS`, and its `label_errors()` (lines 304-332) rejects any
label whose first token is not `tinyland`.

The Ruby port used by `ci-templates lint-runs-on`
(`scripts/runner_label_taxonomy.rb`) carries the same list, and the action
description names the class directly: it "FAILS known repo-shaped /
project-identity self-hosted fossils (e.g. `dollhouse-farm-nix`,
`jesssullivan-nix-heavy`)".

## 2. `tummycrypt-*` IS the scale-set anchor, and it is live

`Jesssullivan/jesssullivan-infra`,
`tofu/stacks/arc-runners/jesssullivan.tfvars`, `extra_runner_sets` (TIN-2538):

```hcl
tummycrypt-nix = {
  github_config_url     = "https://github.com/Jesssullivan/tummycrypt"
  runner_label          = "tinyland-nix"
  runner_scale_set_name = "tummycrypt-nix"
  max_runners           = 3
  runner_image          = "ghcr.io/tinyland-inc/actions-runner-nix@sha256:1ccce66d…"
  node_selector         = { "kubernetes.io/hostname" = "honey" }
}

tummycrypt-dind = {
  github_config_url     = "https://github.com/Jesssullivan/tummycrypt"
  runner_label          = "tinyland-dind"
  runner_scale_set_name = "tummycrypt-dind"
  max_runners           = 1
}
```

The anchor's own comment states the reason it exists: *"Personal-account
repositories cannot consume the tinyland-inc org-scoped scale sets directly, so
these two registration identities expose only the shared workflow-facing
capability labels."*

**Anchor name ≠ label.** The same split is load-bearing across the estate:
`Jesssullivan/bulkload` has `tfvars_anchor: bulkload-nix` but
`runner_class: tinyland-nix`, and its `ci.yml` line 20 reads
`runs-on: tinyland-nix`.

**Liveness, not just declaration.** `jesssullivan-infra` run
`33140300433` (private repo — link elided for link-check)
("Deploy ARC Runners v2", success, 2026-08-28T03:55Z) logs at 04:00:11Z and
04:00:29Z:

```
module.extra_runners["tummycrypt-dind"].helm_release.arc_runner: Refreshing state... [id=tummycrypt-dind]
module.extra_runners["tummycrypt-nix"].helm_release.arc_runner: Refreshing state... [id=tummycrypt-nix]
```

Both Helm releases exist in tofu state. The lane is live today.

## 3. The probe that nearly produced the wrong answer

```
GET /repos/Jesssullivan/tummycrypt/actions/runners
  -> {"total_count":0,"runners":[]}
```

This does **not** mean "no runners". ARC scale sets with `min_runners = 0`
register *ephemeral* runners that exist only while a job is assigned, so the
idle steady state is an empty list. Read alone, this reading supports a
confident and completely wrong conclusion that the repo has no enrollment.
`GET /repos/Jesssullivan/bulkload/actions/runners` returns the same empty list,
and bulkload has been running on `tinyland-nix` for months.

**Use the tofu apply log as the liveness oracle, not the runners API.**

## 4. What moved, and what did not

Migrated to `tinyland-nix` in this PR:

| workflow | job | why it was chosen first |
|---|---|---|
| `nix-ci.yml` | `flake-check` | pure `nix`; fires on every push and PR, so the anchor gets exercised immediately rather than only at release time |
| `nix-ci.yml` | `build-linux-x86_64` | pure `nix`; the runner image is `actions-runner-nix` |
| `release.yml` | `nix-build` | pure `nix`, and a LEAF — nothing `needs:` it, so a bad first run cannot take a release with it |

Deferred, with the specific blocker:

| job | blocker |
|---|---|
| `release.yml` `build-binaries` | `sudo apt-get install` for fuse3/protobuf; the nix runner image is not Ubuntu |
| `release.yml` `create-release` | `sudo dpkg -i` / `sudo rpm -i` / `brew` verification steps |
| `release.yml` `build-image` | needs qemu + buildx; belongs on `tinyland-dind`, and multi-arch qemu inside dind is untested here |
| `release.yml` `plan`, `generate-installers`, `update-homebrew` | depend on hosted-image tooling (`jq`, `gh`, `brew`) that the nix runner image is not known to carry |

No destination at all:

- `release.yml` `build-fileprovider`, `build-pkg`; `nix-ci.yml`
  `build-macos-aarch64`. ARC is Linux-on-Kubernetes. The taxonomy permits
  `macos`/`darwin` **suffixes** on a constructed label, but no such scale set
  exists, and these jobs need real macOS with Xcode and notarization
  credentials. The only self-hosted Mac in the estate is `petting-zoo-mini`,
  itself a repo-shaped fossil label. These stay hosted indefinitely; track
  separately and do not fold them into the Linux flip.

## 5. Sequencing for the rest

1. Let the three migrated jobs run green on `tinyland-nix` at least once.
2. Then move `plan`, `generate-installers`, `update-homebrew` one at a time,
   each with a `command -v` preflight for the tools it assumes.
3. `build-image` → `tinyland-dind` once qemu-in-dind is proven.
4. `build-binaries` and `create-release` need their apt/dpkg/rpm steps replaced
   with nix equivalents first. That is a real piece of work, not a label swap.
5. Lower `.github/runs-on-baseline` in the same commit as each move.

## 6. What this is NOT

Not a GloriousFlywheel `config/consumer-registry.json` entry. That registry is
for Bazel RBE / container-image-builder repos, and its validator
(`scripts/validate-consumer-registry.py`) requires `build_targets` to be a
non-empty list of Bazel labels beginning with `//`, and restricts
`substrate_mode` to `{executor-backed, shared-cache-backed}`. tummycrypt has no
Bazel wiring at all — no `MODULE.bazel`, `BUILD.bazel`, `WORKSPACE.bazel`,
`.bazelrc` or `.bazelversion` anywhere in the tree — so any entry would have to
invent targets, which is precisely the "internal honesty" the registry exists
to enforce. GF's own `docs/build-system/enrollment.md` §6 says it outright:
*"Runner provisioning is outside this registry."*

Runner provisioning for this repo is the owner-overlay anchor in §2, and it is
already done.

Related: TIN-2538 (the anchors), TIN-3914 (hosted-runner ruling), TIN-4050
(personal-repo enrollment).
