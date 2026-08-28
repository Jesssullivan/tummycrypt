# CI runner enrollment: why `runs-on` was NOT migrated to a `tummycrypt-*` class

Date: 2026-08-28. Measured on neo against `origin/main` c782bb9, ci-templates
v3.1.0 (`d8d178c022a0f84853d53a2c8fe0fc90115f0949`), and
GloriousFlywheel `main`.

Lane 4 of the release-flow migration was specified as, among other things,
"`release.yml` `runs-on` → `tummycrypt-*` class". That instruction is not
executable as written, and executing it would break the release lane. This note
records what was measured, so the next person does not re-derive it.

## 1. There is no `tummycrypt-*` runner set

`GloriousFlywheel tofu/stacks/arc-runners/honey.tfvars` defines seven runner
sets. Every one carries a `tinyland-*` `runner_label`:

| `runner_label` | line |
|---|---|
| `tinyland-nix-operator` | 836 |
| `tinyland-nix-merge-gate` | 952 |
| `tinyland-nix` | 1074 |
| `tinyland-dind` | 1166 |
| `tinyland-nix-heavy` | 1270 |
| `tinyland-nix-kvm` | 1374 |
| `tinyland-nix-gpu` | 1479 |

There is no `tummycrypt` anywhere in the file.

## 2. A `tummycrypt-*` label is forbidden by the taxonomy authority

`GloriousFlywheel scripts/validate-arc-runner-taxonomy.py` — the authority for
tinyland tfvars `runner_label` values — lists `tummycrypt` explicitly in
`PROJECT_IDENTITY_TOKENS`, and rejects any label whose first token is not
`tinyland` (`label_errors()`, lines 304-332).

The Ruby port that `ci-templates lint-runs-on` uses
(`scripts/runner_label_taxonomy.rb`) carries the same
`PROJECT_IDENTITY_TOKENS` list, and its action description names the failure
class directly: "FAILS known repo-shaped / project-identity self-hosted
fossils (e.g. `dollhouse-farm-nix`, `jesssullivan-nix-heavy`)".

The `tummycrypt-*` name in the delivery-stack map is best read as an ARS
**anchor** name (`tfvars_anchor`), not a `runs-on` label. That distinction is
already load-bearing elsewhere: in `GloriousFlywheel config/consumer-registry.json`,
`Jesssullivan/bulkload` has `tfvars_anchor: bulkload-nix` but
`runner_class: tinyland-nix`, and `Jesssullivan/bulkload/.github/workflows/ci.yml`
line 20 reads `runs-on: tinyland-nix`.

## 3. Even `tinyland-nix` is unreachable from this repo today

Every runner set in `honey.tfvars` registers against
`github_config_url = "https://github.com/tinyland-inc"` (line 32). GitHub
organization runners serve repositories **in that organization**.
`Jesssullivan/tummycrypt` is a personal-account repository.

Measured:

```
GET /repos/Jesssullivan/tummycrypt/actions/runners
  -> {"total_count":0,"runners":[]}
```

`Jesssullivan/legalab` has already hit this and committed the finding into its
own CI workflow header: *"Personal private repositories cannot consume the
tinyland-inc runner group. TIN-4050 tracks a future repo-scoped PZM/Flywheel
enrollment."*

Flipping any `runs-on:` in this repo to a `tinyland-*` label today would queue
those jobs forever.

## 4. The darwin jobs have no destination even after enrollment

ARC runs on the honey Kubernetes cluster: Linux only. The taxonomy permits
`macos`/`darwin` **suffixes** on a constructed label, but no such runner set
exists. `release.yml`'s `build-fileprovider` and `build-pkg` jobs, and
`nix-ci.yml`'s `build-macos-aarch64`, need real macOS with Xcode and
notarization credentials. The only self-hosted Mac in the estate is
`petting-zoo-mini`, which is itself a repo-shaped fossil label and currently has
zero runners registered against this repository.

So even a successful enrollment closes the Linux half only.

## 5. What was done instead

- `runs-on:` values are unchanged. Changing them to a nonexistent label strands
  the release lane; it does not migrate it.
- The waiver is written into the header of `release.yml` and `nix-ci.yml`, where
  CI is actually read, rather than left as tribal knowledge.
- `.github/workflows/runs-on-contract.yml` (series PR-b) adopts `lint-runs-on`
  as a **ratchet** against `.github/runs-on-baseline`, so the existing debt is a
  single reviewable number and any *new* hosted or repo-shaped `runs-on` fails
  the PR.

## 6. Exit path

1. A GloriousFlywheel PR adding `Jesssullivan/tummycrypt` to
   `config/consumer-registry.json` with `runner_class: tinyland-nix` and
   `tfvars_anchor: tummycrypt-nix` — the bulkload shape. Drafted as series PR-d.
2. The matching owner-overlay extra runner set (`jesssullivan-infra`), so a
   runner actually registers against this repository. Verify with
   `GET /repos/Jesssullivan/tummycrypt/actions/runners` returning a non-zero
   `total_count` carrying the `tinyland-nix` label.
3. Then, and only then, flip the Linux `runs-on:` values, lower
   `.github/runs-on-baseline` in the same commit, and drop the waiver headers
   for the jobs that moved.
4. The darwin jobs stay on hosted runners until a darwin capability class
   exists. Track separately; do not fold it into the Linux flip.

Related: TIN-3914 (hosted-runner ruling), TIN-4050 (repo-scoped enrollment).
