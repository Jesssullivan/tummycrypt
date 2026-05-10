# TCFS neo/honey Conflict Evidence

Created: 2026-05-10T05:46:57Z

This packet targets the same-fixture cross-host conflict row:

1. neo pushes `Projects/shared/conflict-notes.md` to a disposable prefix.
2. honey pulls that file into a physical sync root and edits it locally.
3. neo edits and pushes a different version of the same relative path.
4. honey attempts to push its divergent local version.
5. TCFS must detect conflict, skip the honey upload, mark honey local state as
   `conflict`, preserve honey's local bytes, and leave the remote index at
   neo's last pushed bytes.

When `--honey-recover-keep-both` is enabled, the packet then runs a manual
keep-both recovery pattern:

6. honey copies its conflicted local bytes to `Projects/shared/conflict-notes.conflict-honey.md`.
7. honey pulls the original path back to neo's remote bytes.
8. honey pushes `Projects/shared/conflict-notes.conflict-honey.md`.
9. neo pulls both paths back and compares exact content.

When `--honey-resolve-keep-both` is enabled, the packet instead attempts a
daemon-backed keep-both resolution:

6. honey starts an isolated `tcfsd` with the same disposable config/state as
   the CLI conflict lane and `auth.require_session=false`.
7. honey runs `tcfs resolve --strategy keep-both` for the conflicted path.
8. honey verifies the original path now has neo's remote bytes and the daemon
   conflict copy `Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md` preserves honey's bytes.
9. neo pulls both paths back and compares exact content. If daemon startup or
   resolve wiring blocks the proof, `result.env` records `status=blocked`.

When `--honey-independent-sibling` is enabled, the packet also proves sibling
progress:

6. neo seeds `Projects/shared/conflict-independent-sibling.md`.
7. honey edits that sibling before the conflict.
8. after the original file is conflicted, honey pushes the sibling.
9. neo pulls the sibling and compares exact honey content while the original path
   remains conflicted on honey.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/neo-honey-conflict-daemon-keep-both-20260510T054611Z
```

Result:

`tcfs resolve --strategy keep-both` did not return within the helper's 30-second
timeout after the daemon accepted the request, so this packet is a blocker, not a
daemon-backed resolve proof. The daemon log shows the auth-bypass request was
accepted by `tcfsd 0.12.12`; after timeout, post-blocker pullbacks showed partial
side effects: the original remote path still matched neo's winning bytes and the
daemon-created conflict copy matched honey's losing bytes. The clean RPC/status
completion and user-facing resolution claim remain open.

Important files:

- `neo-initial-push.log`: initial neo publish transcript, when pushed
- `honey-prepare.log`: honey pull/local edit transcript, when run
- `neo-conflict-push.log`: neo divergent push transcript, when run
- `honey-conflict-push.log`: honey conflict push transcript, when run
- `honey-independent-sibling-push.log`: optional independent sibling push transcript
- `honey-keep-both-recovery.log`: optional manual keep-both recovery transcript
- `honey-daemon-resolve-keep-both.log`: optional daemon-backed resolve transcript
- `honey-evidence/`: detailed remote transcripts, copied back when available
- `remote-after-conflict.content`: remote pullback after honey conflict push
- `remote-sibling-after-progress.content`: optional independent sibling pullback
- `remote-original-after-recovery.content`: optional original-path pullback
- `remote-conflict-copy.content`: optional keep-both copy pullback
- `remote-original-after-daemon-resolve.content`: optional daemon-resolved original pullback
- `remote-daemon-conflict-copy.content`: optional daemon-created conflict copy pullback
- `remote-original-after-daemon-timeout.content`: post-blocker original pullback
- `remote-daemon-conflict-copy-after-timeout.content`: post-blocker daemon conflict-copy pullback
- `result.env`: pass/plan-only status

Claimability note: this proves current CLI conflict behavior for one
same-fixture cross-host row. Optional keep-both mode proves a manual recovery
pattern, optional daemon keep-both mode proves or blocks the current
`tcfs resolve` path under an isolated auth-bypass daemon, and optional sibling
mode proves per-path sibling progress while another path is conflicted. These do
not prove authenticated production daemon resolve, Finder conflict UX, automatic
resolution, keep-synced/pin policy, or production FileProvider status badges.
