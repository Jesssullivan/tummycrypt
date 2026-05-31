#!/usr/bin/env python3
"""Regression tests for agentic-flow-mirror-inventory.py."""

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "agentic-flow-mirror-inventory.py"


def load_manifest(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def test_manifest_classification() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "agent-state"
        out = Path(tmp) / "out"
        (root / ".ssh").mkdir(parents=True)
        (root / "node_modules/pkg").mkdir(parents=True)
        (root / ".claude/worktrees/agent-a").mkdir(parents=True)
        (root / "sessions").mkdir()
        (root / "src").mkdir()
        (root / ".ssh/id_ed25519").write_text("secret")
        (root / "auth.json").write_text("{}")
        (root / ".env.local").write_text("secret")
        (root / ".envrc").write_text("use flake")
        (root / "runner.env.j2").write_text("TOKEN=")
        (root / "logs_2.sqlite").write_text("db")
        (root / "logs_2.sqlite-wal").write_text("wal")
        (root / "sessions/demo.jsonl").write_text("{}\n")
        (root / "src/main.rs").write_text("fn main() {}\n")
        (root / ".claude/worktrees/agent-a/README.md").write_text("wip")
        (root / "node_modules/pkg/index.js").write_text("generated")

        subprocess.run([str(SCRIPT), str(root), "--out-dir", str(out)], check=True)
        rows = load_manifest(out / "manifest.jsonl")
        by_name = {Path(str(row["source_path"])).name: row for row in rows}

        assert by_name["auth.json"]["decision"] == "deny"
        assert by_name[".env.local"]["decision"] == "deny"
        assert by_name[".envrc"]["decision"] == "deny"
        assert by_name["runner.env.j2"]["decision"] == "deny"
        assert by_name["logs_2.sqlite"]["decision"] == "snapshot"
        assert by_name["logs_2.sqlite-wal"]["decision"] == "deny"
        assert by_name["demo.jsonl"]["decision"] == "allow"
        assert by_name["main.rs"]["decision"] == "allow"
        assert by_name["node_modules"]["decision"] == "deny"
        assert by_name["worktrees"]["decision"] == "deny"


if __name__ == "__main__":
    test_manifest_classification()
