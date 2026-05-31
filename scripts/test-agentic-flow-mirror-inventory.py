#!/usr/bin/env python3
"""Regression tests for agentic-flow-mirror-inventory.py."""

from __future__ import annotations

import json
import sqlite3
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "agentic-flow-mirror-inventory.py"
PREPARE_SCRIPT = ROOT / "scripts" / "agentic-flow-mirror-prepare.py"


def load_manifest(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def test_manifest_classification() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "agent-state"
        out = Path(tmp) / "out"
        (root / ".ssh").mkdir(parents=True)
        (root / "node_modules/pkg").mkdir(parents=True)
        (root / ".claude/worktrees/agent-a").mkdir(parents=True)
        (root / ".claude/backups").mkdir(parents=True)
        (root / ".codex/tmp/arg0").mkdir(parents=True)
        (root / "sessions").mkdir()
        (root / "src").mkdir()
        (root / ".ssh/id_ed25519").write_text("secret")
        (root / "auth.json").write_text("{}")
        (root / ".env.local").write_text("secret")
        (root / ".envrc").write_text("use flake")
        (root / "runner.env.j2").write_text("TOKEN=")
        with sqlite3.connect(root / "logs_2.sqlite") as conn:
            conn.execute("create table events(id integer primary key, body text)")
            conn.execute("insert into events(body) values ('ok')")
        (root / "logs_2.sqlite-wal").write_text("wal")
        (root / "sessions/demo.jsonl").write_text("{}\n")
        (root / "src/main.rs").write_text("fn main() {}\n")
        (root / ".claude/worktrees/agent-a/README.md").write_text("wip")
        (root / ".claude/backups/.claude.json.backup.1").write_text("{}")
        (root / ".codex/tmp/arg0/prompt").write_text("volatile")
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
        assert by_name["backups"]["decision"] == "deny"
        assert by_name["tmp"]["decision"] == "deny"

        stage = Path(tmp) / "stage"
        subprocess.run(
            [
                str(PREPARE_SCRIPT),
                "--manifest",
                str(out / "manifest.jsonl"),
                "--stage-dir",
                str(stage),
                "--copy-allowed",
                "--snapshot-sqlite",
            ],
            check=True,
        )
        summary = json.loads((stage / "prepare-summary.json").read_text())
        assert summary["copied_files"] == 1
        assert summary["skipped_transcripts"] == 1
        assert summary["snapshots_ok"] == 1
        assert summary["snapshots_failed"] == 0

        staged_main = stage / "files" / Path(*Path(root / "src/main.rs").parts[1:])
        staged_auth = stage / "files" / Path(*Path(root / "auth.json").parts[1:])
        snapshot = stage / "sqlite" / Path(*Path(root / "logs_2.sqlite").parts[1:])
        snapshot = snapshot.with_suffix(".sqlite.snapshot.sqlite")
        assert staged_main.exists()
        assert not staged_auth.exists()
        assert snapshot.exists()


def test_prepare_rejects_unsafe_allow() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "agent-state"
        stage = Path(tmp) / "stage"
        manifest = Path(tmp) / "manifest.jsonl"
        root.mkdir()
        (root / ".env").write_text("SECRET=1")
        manifest.write_text(
            json.dumps(
                {
                    "source_path": str(root / ".env"),
                    "surface_class": "agent_config",
                    "decision": "allow",
                    "reason": "bad-fixture",
                    "size_bytes": 8,
                    "mtime_utc": "2026-05-31T00:00:00Z",
                },
                sort_keys=True,
            )
            + "\n"
        )
        result = subprocess.run(
            [str(PREPARE_SCRIPT), "--manifest", str(manifest), "--stage-dir", str(stage)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        assert result.returncode == 1
        assert "unsafe allow" in result.stderr


if __name__ == "__main__":
    test_manifest_classification()
    test_prepare_rejects_unsafe_allow()
