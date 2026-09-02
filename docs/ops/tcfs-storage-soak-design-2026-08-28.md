# TCFS storage soak design: repeatability, SLO, and retry/noise budget (TIN-1622)

Date: 2026-08-28
Status: design only, not run. Written to close the design half of TIN-1622;
executing it (dispatching the workflow repeatedly against
`tcfs-storage-prod-smoke`) is an operator/CI action, not something this pass
does. See `docs/ops/storage-posture-production-gate.md` for the underlying
gate this soak extends and `docs/BENCHMARKS.md` for the two seed runs.

## Why this exists

`TIN-1621` proved run `26417405494` can restore a package-backed 3.22 GiB
synthetic Git-pack workload *exactly once*, after the same workload failed
on the immediately preceding run `26412362782`. One passing run under heavy
transient noise is alpha-grade recovery evidence, not a beta storage SLO:
there is still no p50/p95 across repeated runs, no stated retry/noise
budget, and no explicit ruling on whether
`https://tcfs-smoke-s3.tinyland.dev` is beta-acceptable or only an alpha
smoke endpoint. This document defines the soak that would answer those
questions and the budget/decision framework to apply to its results;
someone (a human or a future CI-driven pass with dispatch authority) still
has to actually run it.

## Soak procedure

Reuse the existing `storage-large-restore-canary.yml` path unmodified
(`tcfs_binary_source=nix-package`, package-backed `pack_size_mib` sized to
reproduce the ~3.22 GiB `linux-xr-fast` profile, `download_chunk_retries=8`,
`require_https=true`) against the same
`tcfs-storage-prod-smoke` endpoint used by `26412362782`/`26417405494`, so
results are comparable to the seed evidence rather than establishing a new
baseline.

- **N = 10 consecutive runs**, same artifact schema as the seed runs
  (`gha/storage-posture/large/<run_id>-<attempt>`), dispatched at least 30
  minutes apart so back-to-back runs don't share a transient outage window
  and each run gets its own clean 502/retry sample.
- **Do not retry a run that fails the restore-headroom preflight** (host
  capacity, not storage correctness per the existing Failure Classification
  section) -- redispatch instead, and don't count it toward N.
- Archive every run's packet (pass or fail) under
  `docs/release/evidence/`, matching the existing
  `home-canary-linux-xr-storage-posture-*` naming convention, so a failed
  run is preserved as-is rather than only the eventual pass.

## Metrics to record per run

Exactly the fields the ticket names, all already emitted by the existing
workflow/canary command per `docs/BENCHMARKS.md`'s run `26417405494` entry:

- restore elapsed (seconds)
- effective restore throughput (bytes/sec)
- S3/Cloudflare `5xx` log line count
- OpenDAL retry row count
- TCFS chunk-download retry row count
- timeout row count
- socket highwater
- exact-restore verdict (byte-for-byte match against the pushed tree,
  file/dir counts) -- a run that "passes" on wall-clock but silently
  restores a truncated or wrong tree is not a pass

## Aggregation

After N runs, compute across the *passing* runs only (a failing run is a
data point for the retry/noise budget below, not for the SLO number):

- p50 and p95 restore elapsed
- p50 and p95 effective throughput
- mean and max 5xx count, OpenDAL retry count, TCFS retry count
- pass rate = passing runs / N (this is the number the retry/noise budget
  actually gates on)

## Retry/noise budget (draft framework, needs an operator ruling on the numbers)

The seed data point: `26417405494` passed with 668 `5xx` log lines, 289
OpenDAL retries, 47 TCFS chunk retries, and 0 socket highwater, over a
2,823s restore (~1.14 MB/s effective). `26412362782` failed under the same
workload. That is exactly one sample on each side of the line -- not enough
to set a budget, which is the entire reason this soak exists.

Proposed decision bands (to be filled in with the actual N=10 results, not
assumed):

| Signal | Alpha-acceptable | Forces endpoint/client change | Blocks beta release |
| --- | --- | --- | --- |
| Pass rate across N=10 | \>= 70% | 40-70% | < 40% |
| p95 5xx count (passing runs) | within ~2x the seed run's 668 | 2x-5x | \> 5x with no downward trend across the soak |
| p95 OpenDAL retry count | within ~2x the seed run's 289 | 2x-5x | \> 5x |
| Any run with socket highwater \> 0 | none observed | isolated (1 run) | recurring (\>= 2 runs) -- indicates connection-pool exhaustion, not transient 5xx noise |
| p95 effective throughput | \>= 0.5 MB/s | 0.2-0.5 MB/s | < 0.2 MB/s |

"Forces endpoint/client change" means: before beta, either
`tcfs-storage-prod-smoke`'s backend needs different capacity/config, or the
TCFS client's `download_chunk_retries`/backoff needs tuning -- ship neither
claim without doing one or the other and re-running a smaller confirming
soak.

## Endpoint decision

`https://tcfs-smoke-s3.tinyland.dev` is provisionally **alpha-smoke-only**
until the N=10 soak's pass rate and p95 numbers land in the
alpha-acceptable band above. If they land in "blocks beta release," that is
itself the answer TIN-1622 asks for: the endpoint decision is "not
beta-acceptable as configured," and the next step is capacity/config work
on the endpoint, not another soak against the same unmodified backend.

## Scope boundary (unchanged from the ticket)

Explicitly out of scope for this soak and this document, same as the
ticket's own scope line: broad home-directory claims, multitenant claims,
lost-device recovery, and any other uncaveated daily-driver claim. This
soak is scoped to the package-backed large-restore path against one
endpoint, nothing broader.

## What to update once the soak actually runs

- `docs/BENCHMARKS.md`: add a "Soak (N=10)" section next to the existing
  `26417405494`/`26412362782` entries with the p50/p95 table above filled
  in from real data.
- `docs/ops/storage-posture-production-gate.md`: update the "Relationship
  To Alpha/Beta Claims" section with the endpoint ruling and reference this
  soak's evidence directory.
- the daily-driver todo (`docs/ops/tcfs-daily-driver-productionization-todo-2026-05-24.md`
  or its current-dated successor): mark the retry/noise budget decided, or
  explicitly still-blocked, with a link to the archived evidence.

## What this document does not do

It does not run the soak, does not report real p50/p95 numbers (the table
above is a decision framework with placeholder bands, not a result), and
does not itself change the endpoint's alpha/beta status -- that ruling only
becomes real once N=10 real runs exist. This is deliberate: TIN-1622 asks
this pass to design the soak, not execute it locally or fabricate results.
