#!/usr/bin/env python3
"""Regression tests for scripts/large-workdir-inventory.py."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "large-workdir-inventory.py"


def run_inventory(root: Path, out_dir: Path) -> dict[str, object]:
    subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            str(root),
            "--out-dir",
            str(out_dir),
            "--skip-xattrs",
        ],
        check=True,
        cwd=REPO_ROOT,
    )
    return json.loads((out_dir / "inventory.json").read_text())


def main() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        root = base / "candidate"
        out_dir = base / "packet"
        root.mkdir()
        (root / "README.md").write_text("hello\n")
        (root / ".agent-state").mkdir()
        (root / ".agent-state" / "state.json").write_text("{}\n")
        (root / "nested").mkdir()
        (root / "nested" / "data.bin").write_bytes(b"abc")
        try:
            (root / "readme-link").symlink_to("README.md")
            expect_symlink = True
        except OSError:
            expect_symlink = False
        try:
            os.mkfifo(root / "events.fifo")
            expect_special = True
        except (AttributeError, OSError):
            expect_special = False

        subprocess.run(["git", "init", "-q"], cwd=root, check=True)

        data = run_inventory(root, out_dir)

        assert data["root"] == str(root.resolve())
        assert data["regular_files"] >= 3, data
        assert data["directories"] >= 4, data
        assert data["hidden_directories"] >= 2, data
        assert data["total_bytes"] >= 9, data
        assert data["git_present"] is True, data
        assert data["git_dirty"] is True, data
        assert data["git_status_entries"] >= 1, data
        if expect_symlink:
            assert data["symlinks"] == 1, data
        if expect_special:
            assert data["special_files"] == 1, data
            assert data["recommendation"] == "blocked_special_files", data
        assert (out_dir / "inventory.env").exists()
        summary = (out_dir / "summary.md").read_text()
        assert "read-only inventory" in summary

    print("large workdir inventory tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
