# TCFS neo/honey Daemon Keep-Both Blocker Evidence

Created: 2026-05-10T05:43:00Z

This packet is a superseded daemon-backed `tcfs resolve --strategy keep-both`
attempt. The conflict setup succeeded: honey pulled and edited
`Projects/shared/conflict-notes.md`, neo pushed divergent bytes, and honey's
push recorded conflict state without overwriting neo's remote bytes.

The daemon resolve step is not claimed. Taskfile did not forward
`HONEY_TCFSD_BIN`, so honey selected `tcfsd` from PATH:

```text
/nix/store/fk0d3yx8py43qza99sfx2czyvz416pyi-tcfsd-0.12.2/bin/tcfsd
```

The stale daemon accepted the request and logged `conflict resolution requested`
but the CLI resolve call did not complete before the attempt was terminated.
This packet is retained only as stale-daemon/task-wiring blocker evidence.

Important files:

- `run-metadata.env`: records `honey_tcfsd_bin=tcfsd`
- `honey-evidence/honey-tcfsd-resolve-keep-both.log`: stale daemon startup and request log
- `honey-evidence/honey-daemon-resolve-keep-both.out`: empty/unfinished resolve transcript
- `honey-conflict-push.log`: conflict detection transcript
- `remote-after-conflict.content`: remote pullback proving neo bytes survived honey conflict push
- `result.env`: blocker summary

Claimability note: this does not prove daemon-backed `tcfs resolve`, Finder
conflict UX, authenticated production daemon resolve, or automatic resolution.
