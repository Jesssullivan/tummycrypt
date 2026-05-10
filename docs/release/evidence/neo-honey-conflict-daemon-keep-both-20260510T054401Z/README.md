# TCFS neo/honey Daemon Keep-Both Pre-Timeout Blocker Evidence

Created: 2026-05-10T05:46:00Z

This packet is a superseded daemon-backed `tcfs resolve --strategy keep-both`
attempt using the freshly built honey `tcfsd 0.12.12` binary. The conflict setup
completed and honey recorded conflict state without overwriting neo's remote
bytes.

The resolve call hung after `tcfsd` accepted the keep-both request. This attempt
was terminated manually before the helper had timeout handling, so it is retained
only as pre-timeout blocker evidence and is superseded by
`neo-honey-conflict-daemon-keep-both-20260510T054611Z/`.

Important files:

- `run-metadata.env`: records the explicit honey `tcfsd` binary
- `honey-evidence/honey-tcfsd-resolve-keep-both.log`: daemon startup and accepted request log
- `honey-evidence/honey-daemon-resolve-keep-both.out`: empty/unfinished resolve transcript
- `honey-conflict-push.log`: conflict detection transcript
- `remote-after-conflict.content`: remote pullback proving neo bytes survived honey conflict push
- `result.env`: blocker summary

Claimability note: this does not prove daemon-backed `tcfs resolve`, Finder
conflict UX, authenticated production daemon resolve, or automatic resolution.
