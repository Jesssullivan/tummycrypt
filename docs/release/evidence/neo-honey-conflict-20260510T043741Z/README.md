# TCFS neo/honey Conflict Evidence

Created: 2026-05-10T04:37:51Z

This packet targets the same-fixture cross-host conflict row:

1. neo pushes `Projects/shared/conflict-notes.md` to a disposable prefix.
2. honey pulls that file into a physical sync root and edits it locally.
3. neo edits and pushes a different version of the same relative path.
4. honey attempts to push its divergent local version.
5. TCFS must detect conflict, skip the honey upload, mark honey local state as
   `conflict`, preserve honey's local bytes, and leave the remote index at
   neo's last pushed bytes.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/neo-honey-conflict-20260510T043741Z
```

Important files:

- `neo-initial-push.log`: initial neo publish transcript, when pushed
- `honey-prepare.log`: honey pull/local edit transcript, when run
- `neo-conflict-push.log`: neo divergent push transcript, when run
- `honey-conflict-push.log`: honey conflict push transcript, when run
- `honey-evidence/`: detailed remote transcripts, copied back when available
- `remote-after-conflict.content`: remote pullback after honey conflict push
- `result.env`: pass/plan-only status

Claimability note: this proves current CLI conflict behavior for one
same-fixture cross-host row. It does not prove Finder conflict UX, automatic
resolution, keep-synced/pin policy, or production FileProvider status badges.
