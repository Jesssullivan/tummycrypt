# Neo → Sting Bulkload R23/R25 Evidence — 2026-09-18

## Verdict

**R23 failed; estate transport and apply remain blocked.** The corrected native
initial-copy median was `3015.294 ms`, versus rclone `601.010 ms`. Native won
the corrected 1%-of-regular-file-bytes delta median (`58.842 ms` versus
`127.064 ms`), but both R23 comparisons are mandatory.

**R25 passed.** Every unchanged native warm sample and the injected interrupted
resume reported both `source_bytes_read=0` and `transferred_content_bytes=0`.
The cumulative process peak reported for native was `234400 KiB`, below the
2 GiB limit. All samples completed full-BLAKE3 verification outside timing.

## Identity and Command

- Revision: `c6cead96f325+worktree-d7f9f8d33baf0e99` (HEAD plus SHA-256 prefix
  of the binary source diff).
- Sealed corpus: `/Users/jess/git/tummycrypt/.bulkload-bench-real-ee15653/corpus`.
- Sealed identity: `9b52260a0716ee6aebdf1a6725f08a8316f7c89e0379f76bcb1156859846e39b`.
- Private pre-delta identity: `1b7f79d623f52c147d18bac2201b318fbc2f1a81fd718398f35772a3ecb876ab`.
- Private post-delta identity: `67c063fabd977dc15204fd658b32b25ef844407b9a7d20de9488376a9bc45c31`.
- rclone: `/Users/jess/.nix-profile/bin/rclone`, `rclone v1.75.0`.
- rclone arguments: `copy SOURCE DESTINATION --config /dev/null
  --create-empty-src-dirs --links --metadata --transfers 4 --checkers 4
  --stats 0 --log-level ERROR`.
- Work root: `/Users/jess/git/tummycrypt/.bulkload-bench-real-singlepub-20260918-2318`.
- Cache was not flushed. RSS values are cumulative process peaks for native and
  cumulative child-process peaks for rclone, not resettable per-arm samples.

```text
target/release/tcfs-bulkload-bench \
  --corpus-root /Users/jess/git/tummycrypt/.bulkload-bench-real-ee15653/corpus \
  --work-root /Users/jess/git/tummycrypt/.bulkload-bench-real-singlepub-20260918-2318 \
  --rclone /Users/jess/.nix-profile/bin/rclone \
  --reps 3 \
  --revision c6cead96f325+worktree-d7f9f8d33baf0e99
```

The private delta deterministically changed `2,426,057` of `242,605,606`
regular-file bytes in one file (exactly `ceil(total / 100)`). Only that path
was removed from each private destination outside timing so both arms could
reconstruct it without weakening no-clobber semantics. The sealed corpus
identity remained unchanged.

## Raw Samples

`transferred` and `source_read` are unavailable for rclone. Throughput uses the
declared logical workload bytes and integer bytes/second. Verification happened
outside each elapsed interval.

| sequence | arm | phase | elapsed ms | throughput B/s | workload bytes | transferred | source read | RSS KiB |
|---:|---|---|---:|---:|---:|---:|---:|---:|
| 0 | Native | initial | 3430.542 | 70,719,320 | 242,605,606 | 226,762,914 | 242,605,606 | 193,328 |
| 1 | rclone | initial | 675.829 | 358,974,927 | 242,605,606 | unknown | unknown | 55,776 |
| 2 | Native | initial | 3015.294 | 80,458,355 | 242,605,606 | 226,762,914 | 242,605,606 | 234,400 |
| 3 | rclone | initial | 526.192 | 461,059,457 | 242,605,606 | unknown | unknown | 55,776 |
| 4 | Native | initial | 2938.198 | 82,569,510 | 242,605,606 | 226,762,914 | 242,605,606 | 234,400 |
| 0 | Native | warm-resume | 8.450 | 0 | 0 | 0 | 0 | 234,400 |
| 1 | rclone | warm-resume | 191.545 | 0 | 0 | unknown | unknown | 55,776 |
| 2 | Native | warm-resume | 8.468 | 0 | 0 | 0 | 0 | 234,400 |
| 3 | rclone | warm-resume | 44.367 | 0 | 0 | unknown | unknown | 55,776 |
| 4 | Native | warm-resume | 9.745 | 0 | 0 | 0 | 0 | 234,400 |
| 0 | Native | interrupted-resume | 3.367 | 0 | 0 | 0 | 0 | 234,400 |
| 0 | Native | delta | 53.827 | 45,071,690 | 2,426,057 | 2,444,912 | 3,280,458 | 234,400 |
| 1 | rclone | delta | 203.212 | 11,938,522 | 2,426,057 | unknown | unknown | 55,776 |
| 2 | Native | delta | 58.842 | 41,229,905 | 2,426,057 | 2,444,912 | 3,280,458 | 234,400 |
| 3 | rclone | delta | 50.916 | 47,648,341 | 2,426,057 | unknown | unknown | 55,776 |
| 4 | Native | delta | 68.392 | 35,472,926 | 2,426,057 | 2,444,912 | 3,280,458 | 234,400 |

## Native Phase Counters

Durations below are cumulative process worker sums; concurrent phases can
overlap and must not be added to infer wall time.

| sequence | phase | CDC/hash ns | queue wait ns | transfer ns | materialize ns | publish groups | pack append ns | file syncs | file sync ns | SQLite commits | SQLite commit ns |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | initial | 3,083,430,623 | 2,469,319,042 | 1,476,510,456 | 1,171,320,332 | 38 | 386,544,038 | 32 | 744,585,583 | 32 | 41,088,793 |
| 2 | initial | 2,714,775,458 | 2,133,558,208 | 1,251,768,668 | 1,025,400,334 | 39 | 604,772,335 | 32 | 275,288,873 | 33 | 41,050,207 |
| 4 | initial | 2,986,573,292 | 2,465,453,630 | 1,270,028,415 | 1,073,846,458 | 39 | 489,803,204 | 33 | 315,227,126 | 33 | 39,466,997 |
| 0 | delta | 4,556,500 | 2,917 | 17,961,500 | 13,192,750 | 2 | 751,208 | 2 | 12,277,667 | 2 | 1,817,458 |
| 2 | delta | 4,776,959 | 3,709 | 16,874,750 | 19,848,542 | 2 | 811,000 | 2 | 8,691,959 | 2 | 1,528,916 |
| 4 | delta | 4,972,041 | 2,500 | 23,642,166 | 19,766,083 | 2 | 1,148,583 | 2 | 11,156,791 | 2 | 1,147,791 |

## Medians and Gate

| gate | native | rclone | outcome |
|---|---:|---:|---|
| R23 initial | 3015.294 ms | 601.010 ms | **fail** |
| R23 1%-byte delta | 58.842 ms | 127.064 ms | pass |
| R25 warm | 0 bytes read/received | n/a | pass |
| R25 interrupted resume | 0 bytes read/received | n/a | pass |
| Native cumulative peak RSS | 234,400 KiB | n/a | pass |

The benchmark exited nonzero because the combined R23/R25 gate failed. No
estate payload was transported or applied.
