# TCFS neo/honey Reverse Unsynced Rehydrate Attempt

Created: 2026-05-10T02:26:57Z

This was the first live reverse same-fixture M6 attempt. It is blocker
evidence, not a pass.

The flow reached the important failure condition:

1. neo pushed `Projects/shared/reverse-notes.md` to a disposable prefix.
2. honey pulled the file into a physical sync root and ran `tcfs unsync`.
3. neo mutated and pushed the same relative path.
4. honey pulled the updated 107-byte neo content and reported
   `sync state: synced`.
5. honey still had the adjacent `.tc` stub after pull.

The failure was caused by using the older honey Linux `tcfs` build from
`/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs`, which did not
include the pull-side adjacent-stub cleanup added in this sprint.

The passing rerun is:

```text
docs/release/evidence/neo-honey-reverse-unsynced-rehydrate-20260510T022858Z/
```
