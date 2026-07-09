# TCFS Vision — the remote-first userspace filesystem

> This is the front door for *why* TCFS exists. It states the north star and
> holds every capability claim to a proof tier, so the vision cannot quietly
> drift into a promise. For *where the work actually is right now*, always read
> [`ops/current-workstream-truth-2026-07-06.md`](ops/current-workstream-truth-2026-07-06.md)
> next — this doc is the direction, that doc is the odometer.

**Claim-tier legend (house style — every claim below carries one):**

- **PROVEN-LIVE** — reproduced on real hosts with a dated `docs/release/evidence/`
  packet or a `task lazy:*` canary.
- **MERGED-UNPROVEN** — code is on `main` but has no live re-proof packet yet.
- **IN-PR** — designed and under review, not merged.
- **DESIGNED-ONLY** — a design exists; no landed implementation.
- **NOT-DESIGNED** — named as intent; no design yet.

Do not inflate a claim past its tier. The credibility of this document *is* the
point — an honest "not yet" is worth more than an aspirational "done."

## The north star

TCFS (the TummyCrypt Filesystem) is being built to become the **core
remote-first userspace filesystem for the tinyland fleet** and the designated
sync/roam layer for the **rockies Rocky-10 workstation program**. The shape of
the fleet is many small netbooks pushing work onto many heterogeneous servers:
`neo` (a macOS netbook, the primary pusher) roams in-progress work onto `honey`
(the Rocky 10 backbone and first roam target), `bumble` (Rocky 10.1, 22 TB, the
acceptance ladder's third host), and later `sting` (a Rocky HA pair). `blahaj`
remains the GitOps/edge authority — deliberately *not* an enrollable host. The
end state: you `ssh` into any machine in the fleet and your git repos, your
agent dotdirs, and your in-progress files are already there, byte-exact and
end-to-end encrypted, with no additional effort.

Concretely, the daily-driver claim is that git repositories (branch, index,
untracked files, stashes — and eventually linked worktrees), agent state
directories (`~/.claude`, `~/.codex`), and open working files are byte-exact
wherever you land, E2E-encrypted in transit and at rest, with selective-sync
verbs to *unsync-from-neo* and *live-on-honey* on demand. Content is
**hydratable anywhere**: a machine can carry a lightweight representation of a
tree and pull the real bytes lazily, only when something opens them, then
release that space again when it does not need them. The differentiator versus
odrive/Dropbox is not the surface — it is that the substrate is trustworthy by
construction: modern per-chunk encryption (XChaCha20-Poly1305 / Argon2id),
content-addressed chunks (FastCDC + BLAKE3), vector-clock conflict detection,
open self-hosted storage, and platform-native placeholders where the platform
offers them.

## What this means concretely

Three user stories carry the whole vision. Each is stated with its current tier.

- **ssh-anywhere (repo roam).** Enroll a `~/git` repo once; your complete
  in-progress dev environment — current branch, staged and unstaged edits,
  untracked files, and stashes, plus full history — follows you to every
  enrolled host. `ssh` in, `cd` into the repo, pick up exactly where you left
  off. **PROVEN-LIVE (forward, one-way):** the `neo → honey` forward roam of a
  real repo's full dev-env state is fingerprint-identical and `git fsck` clean
  on both sides (2026-06-09,
  [`release/evidence/repo-roam-canary-20260609/`](release/evidence/repo-roam-canary-20260609/);
  see the README's
  [Roam an in-progress repo across machines](../README.md#roam-an-in-progress-repo-across-machines)).
  The proven mechanism is a scheduled CLI/daemon `tcfs reconcile` that syncs the
  whole repo *including `.git`* as ordinary encrypted files — **not** a
  desktop-integration write path.
- **hydrate-anywhere (lazy cloud files).** A host can browse a remote-backed
  tree without paying to download file bodies, then hydrate exact content on
  open and release it again with `unsync`. **PROVEN-LIVE on Linux FUSE:**
  browse-before-download, exact `cat` hydration, mounted write/readback, cache
  clear/rehydrate, and recursive safe-unsync are host-proven on x86_64. Making
  the same transparent hydrate work on plain (non-mount) sync roots is **NOT
  yet proven** (TIN-2683).
- **home-dir remotification.** The long arc is that selected parts of `~` —
  agent dotdirs first, then chosen `~/git` subtrees — live on the fabric with
  per-host selective sync, so `honey` can become a daily driver. This is a
  *sequenced* goal, not a switch: broad `~/git`, `~/Documents`, dotdir, or
  home-directory ownership stays **out of claim** until two repos pass the
  acceptance ladder in both directions (the stop rule inherited from TIN-1617).
  Device **self-onboarding** — a fresh machine joining without an operator
  hand-carrying credentials — is **NOT yet proven** (TIN-2681).

The honest boundary that keeps this document credible: two hosts editing the
*same* repo concurrently do not yet auto-converge into a clean shared state via
an operator verb. `.git`-aware conflict *safety* on a disposable fixture is
proven (see the ladder section), but **production bidirectional convergence is
NOT yet claimed** (TIN-2658).

## Client lanes and their proof tiers

TCFS reaches users through several client surfaces. They are at very different
maturities, and the vision does not pretend otherwise.

| Lane | Platform | What it is | Tier today |
| --- | --- | --- | --- |
| **CLI / daemon** (`tcfs`, `tcfsd`) | Linux, macOS | push/pull/reconcile/policy/unsync; carries the proven repo-roam path | **PROVEN-LIVE on Linux**; macOS install-smoke proven, not continuously acceptance-tested |
| **FUSE mount** | Linux | clean-name on-demand hydration; the lazy-cloud-files surface | **PROVEN-LIVE** (x86_64 lifecycle host-proven; packaged install-to-mount is a separate gate) |
| **FileProvider** | macOS, iOS | Finder/Files-native placeholders and hydration | macOS lifecycle (hydrate, evict/rehydrate, mutation, conflict-status) **PROVEN-LIVE on the PZM Developer-ID lab lane**; first-run UX, badges/progress, and continuous production write-path remain **open**; iOS is proof-of-concept with **unproven** write hooks |
| **NFS loopback** | Linux | FUSE-free mount alternative (no kernel modules) | **MERGED-UNPROVEN** — interface exists; release evidence pending |
| **CloudFilter (CFAPI)** | Windows | Explorer placeholder provider | **DESIGNED-ONLY / skeleton** — 10 CFAPI TODOs before functional; no CI matrix |

Surface strategy across Apple platforms is tracked in the RFC pair (see
Pointers). Per-lane maturity detail lives in
[`platform-support.md`](platform-support.md) and, for the honest product
posture, [`ops/product-reality-and-priority.md`](ops/product-reality-and-priority.md).

## The rockies coupling

TCFS is the "Sync/roam: TCFS" row in the rockies Rocky-10 OS architecture
(`docs/xr-os-architecture-zero-2026-07-06.md` in the `tinyland-inc/rockies`
repo). The integration is deliberately seeded from the rockies side so the
adoption record has one home and does not fork:

- **TIN-2300** — the rockies adoption seed: `rockies` carries
  `manifests/dependencies/tcfs.yaml` (with honest carry-status: *no Rocky RPM
  export lane exists yet*), a profile entry referencing it, and a required
  tracked follow-on for packaging. As of the 2026-07-01 exploration, **zero
  `tcfs` mentions existed in `~/git/rockies`** — this is a seed, not a claim of
  integration.
- **TIN-2688** — the tummycrypt-side follow-on TIN-2300's own acceptance
  demands: define the **Rocky RPM/FUSE packaging lane**. Today the only proven
  RPM path is **daemon-only and Fedora-tested**; a Rocky 10 / 10.1 RPM row
  (daemon-only or FUSE-inclusive — pick one and justify) plus at least one Rocky
  CI smoke run is required work, **not** existing capability.

The adoption record and dependency manifest live in the **rockies** repo by
design; this document is the tummycrypt-side statement of intent that TIN-2300
references, so the vision has a single source of truth instead of drifting
between Linear and two repos.

## Where we are on the ladder

Daily-driver readiness is gated by the **G0–G6 ladder** defined in
[`ops/large-workdir-daily-driver-sequencing-2026-05-30.md`](ops/large-workdir-daily-driver-sequencing-2026-05-30.md)
(design seed:
[`ops/large-workdir-onboarding-design-2026-05-25.md`](ops/large-workdir-onboarding-design-2026-05-25.md)),
plus a four-pillar bar (restore / recovery / security / user-visible control),
each requiring a dated evidence packet. Approximate current standing — but the
**authoritative, dated blocker list is
[`ops/current-workstream-truth-2026-07-06.md`](ops/current-workstream-truth-2026-07-06.md)**,
not this summary:

- **G0 — guardrails.** The fail-closed `Blacklist` deny-set (secrets, `.env`,
  live sqlite/WAL) is shipped and tested. **MERGED / enforced.**
- **G1 — per-device crypto.** Per-prefix FileKey rotation and real revocation
  are the active direction (TIN-2551 under TIN-1417); PerDevice wrapping is the
  committed exit. **IN-PROGRESS / DESIGNED.**
- **G2 + G3 — backbone and honey as device #2.** The `neo ⇄ honey` backbone is
  live and honey is enrolled. **PROVEN-LIVE.**
- **G4 — agent-dir beachhead** (`~/.claude/projects`). Roaming agent state is
  the first live cross-host directory class. **In flight.**
- **G5 — one expendable live repo** (TIN-1620). Forward roam is **PROVEN-LIVE**
  (above). Fast-forward `.git` conflict resolution is closed (#513); the
  divergent keep-both no-loss stack is merged (through #534) and **`.git`
  conflict safety on a disposable fixture is proven** (G5-git-5, evidence
  `051b119`) via the automatic loser-side no-loss guard. The **operator resolve
  verb** and **production bidirectional convergence are NOT yet claimed**
  (blocked by TIN-2653 / TIN-2657; tracked under TIN-2658).
- **G6 — selective-sync verb loop** (`tcfs sub add/remove/list`). Lands
  independently of the beachhead. **IN-PROGRESS.**

The critical path to the beachhead is `G1 + G2 → G3 → G4`, with `G0` a parallel
hard precondition. No broad `~/git`, `/tmp`, or home-takeover claim is made
until two repos clear the acceptance rows in **both** directions.

## Pointers

- **Prompt queue — prompt 47.** This vision is carried as an enqueued front-door
  prompt in the external `prompts-enqueue` lane (prompt 47); this file is the
  in-repo landing it points at.
- **Cordillera — Tinyland Remote-Everything Program** (Linear initiative
  `cordillera-tinyland-remote-everything-program`). The evergreen umbrella over
  the remote-everything gestalt — Rockies OS, the GloriousFlywheel RE substrate,
  the XR/BCI Lab, and the TCFS/tummycrypt fabric. TCFS is the storage/roam range
  in that system; the cross-links *are* the structure.
- **Fleet vision, source of truth.** The full vision text (encoded 2026-07-01,
  red-team-validated) lives on the Linear **Tummycrypt — Daily Driver Track**
  initiative (`tummycrypt-daily-driver-track`). TIN-2687 tracks mirroring it
  here so it stops being Linear-only.
- **RFC pair — the client-surface architecture.**
  [`rfc/0002-darwin-fuse-strategy.md`](rfc/0002-darwin-fuse-strategy.md)
  (FileProvider as the primary macOS/iOS path) and
  [`rfc/0004-fuse-free-architecture.md`](rfc/0004-fuse-free-architecture.md)
  (the FUSE-free target architecture) frame how the client lanes above evolve.
- **Product behavior target.**
  [`ops/odrive-parity-product-horizon.md`](ops/odrive-parity-product-horizon.md)
  — odrive parity as a *user-behavior* bar, not an implementation to copy.
- **Related tickets:** TIN-1617 (G0–G6 ladder), TIN-1620 (expendable live repo),
  TIN-2300 (rockies adoption), TIN-2688 (Rocky RPM/FUSE lane), TIN-2658
  (production bidirectional convergence), TIN-2681 (device self-onboarding),
  TIN-2683 (transparent hydrate on plain sync roots).
