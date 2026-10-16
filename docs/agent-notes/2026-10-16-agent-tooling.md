# 2026-10-16: agent-tooling contract correction

Lane: agent-tooling fix, branch `docs/agent-tooling-20261016` off
`origin/main` `11fc85e`.

Rulings: R-HOOK-CONVERGENCE-20261004 (Linear TIN-3692 comment `98cf680c`,
amended R-N11 and R-N12), R-N13, and the operator's 2026-10-16 statement
"force push is totally allowe[d]".

## Done

- `AGENTS.md` "Ratified rulings": the superseded R-N11 blanket process ban
  and R-N12 "a guard-hook refusal is a stop" block are replaced by the
  R-HOOK-CONVERGENCE-20261004 block, plus the force-push statement. The
  "these three rulings outrank everything else" sentence is gone.
- Agent Coordination: the `codex/*` "codex-owned, don't touch" lane claim is
  restated as an amended-R-N11 cross-session authorization rule. Durable
  session notes now point at `docs/agent-notes/`; `docs/ops/` stays for
  runbooks. Instruction precedence puts ratified operator rulings above this
  file and names the root `CLAUDE.md` import (`.claude/` is gitignored).
- `docs/ops/remote-governance.md`: "force-push on feature branches only by
  the author and only before review" becomes "force push is allowed";
  `main` stays governed by server-side branch protection.

## Open

- Dead `origin/codex/*` branches: deleting them is a branch deletion on a
  public repo and needs an explicit operator ruling.
- Local checkout state (a `core.hooksPath` override, a detached stale root
  checkout, untracked `.junie` plans and `.serena/project.yml`) is not part
  of this PR.
