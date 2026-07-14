# Documentation archive

This directory contains dated context that is intentionally absent from active
product navigation. Nothing here is an operator instruction or current product
claim.

The obsolete January 2025 Go/Chapel/Ansible bundle was removed on 2026-07-14.
Each file had a distinct historical role:

- `AGENTS.md` described the superseded Ansible/SeaweedFS deployment and was
  removed so it cannot act as an instruction overlay.
- `CRUSH.md` was the primary Go/Chapel architecture specification, including
  Crush-derived auth, CLI/TUI, SeaweedFS, build, test, and security patterns.
- `DEVELOPMENT.md` was the matching Go/Chapel/Podman workstation and local-stack
  setup guide.
- `RESEARCH.md` surveyed S3 sync and conflict strategies, Chapel parallel I/O,
  TPM/KeePassXC/GitLab enrollment, and SeaweedFS APIs.
- `RETOOL.md` was the original charter for replacing the Ansible deployment
  with a secure Go/Chapel S3 synchronization system.

Its last content-changing commit is
`dbe8b776f05b85836fb50459179bede4ab98344b`; the last complete tree before
this cleanup is `21f8df303596d1b9f6f90cc7953eb8f65f353ac3`. Git retains the
complete text and history. These pinned commands recover the exact removed
versions:

```bash
git show 21f8df303596d1b9f6f90cc7953eb8f65f353ac3:docs/archive/AGENTS.md
git show 21f8df303596d1b9f6f90cc7953eb8f65f353ac3:docs/archive/CRUSH.md
git show 21f8df303596d1b9f6f90cc7953eb8f65f353ac3:docs/archive/DEVELOPMENT.md
git show 21f8df303596d1b9f6f90cc7953eb8f65f353ac3:docs/archive/RESEARCH.md
git show 21f8df303596d1b9f6f90cc7953eb8f65f353ac3:docs/archive/RETOOL.md
```

The active sources are [the documentation index](../index.md),
[the architecture overview](../ARCHITECTURE.md), and
[the current workstream](../ops/current.md).
