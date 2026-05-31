#!/usr/bin/env python3
"""Create a read-only TCFS mirror-readiness manifest for agentic-flow state."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path


DENY_DIRS = {
    ".cache",
    ".direnv",
    ".git",
    ".gnupg",
    ".ssh",
    ".tmp",
    ".venv",
    "node_modules",
    "nix/secrets",
    "sops-nix",
    "target",
}
DENY_FILES = {
    ".credentials.json",
    ".netrc",
    ".pgpass",
    "auth.json",
    "mcp-auth.json",
    "mcp-needs-auth-cache.json",
    "session-env",
}
DB_SUFFIXES = (".sqlite", ".sqlite3", ".db")
DB_SIDECAR_SUFFIXES = (
    ".sqlite-wal",
    ".sqlite-shm",
    ".sqlite3-wal",
    ".sqlite3-shm",
    ".db-wal",
    ".db-shm",
    "-wal",
    "-shm",
)
GENERATED_DIRS = {".artifacts", ".svelte-kit", "build", "dist", "result"}


@dataclass
class ManifestRow:
    source_path: str
    surface_class: str
    decision: str
    reason: str
    size_bytes: int
    mtime_utc: str
    hash_sha256: str = ""
    repo_branch: str = ""
    repo_dirty_summary: str = ""
    remote_summary: str = ""
    snapshot_source: str = ""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Create a read-only prepare-only manifest for TCFS agentic-flow mirrors."
    )
    parser.add_argument(
        "sources",
        nargs="+",
        type=Path,
        help="Files or directories to inventory without following symlinks",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        required=True,
        help="Directory for manifest.jsonl and summary.json",
    )
    parser.add_argument(
        "--hash-allowed",
        action="store_true",
        help="Hash allowed regular files. Denied and snapshot-required files are never hashed.",
    )
    return parser.parse_args()


def utc_mtime(path: Path) -> str:
    try:
        ts = path.stat(follow_symlinks=False).st_mtime
    except OSError:
        return ""
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def file_size(path: Path) -> int:
    try:
        st = path.stat(follow_symlinks=False)
    except OSError:
        return 0
    return st.st_size if path.is_file() else 0


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def is_dotenv(name: str) -> bool:
    return name == ".env" or name.startswith(".env.") or name.endswith(".env")


def has_component(path: Path, candidates: set[str]) -> str | None:
    parts = path.parts
    for item in candidates:
        item_parts = tuple(Path(item).parts)
        if len(item_parts) == 1:
            if item in parts:
                return item
            continue
        for idx in range(0, len(parts) - len(item_parts) + 1):
            if tuple(parts[idx : idx + len(item_parts)]) == item_parts:
                return item
    return None


def classify(path: Path, root: Path) -> tuple[str, str, str]:
    rel = path.relative_to(root) if path != root else Path(path.name)
    name = path.name

    denied_dir = has_component(rel, DENY_DIRS)
    if denied_dir:
        return "deny", "secret-or-runtime-dir", denied_dir

    generated_dir = has_component(rel, GENERATED_DIRS)
    if generated_dir:
        return "deny", "generated-output", generated_dir

    if name in DENY_FILES or is_dotenv(name):
        return "deny", "credential-or-env", name

    if name.endswith(DB_SIDECAR_SUFFIXES):
        return "deny", "live-db-sidecar", name

    if name.endswith(DB_SUFFIXES):
        return "snapshot", "sqlite-backup-required", name

    if name.endswith(".log"):
        return "deny", "runtime-log", name

    if path.is_symlink():
        return "deny", "symlink-deferred", name

    if name.endswith(".jsonl"):
        return "allow", "append-log-active-writer-check-required", name

    return "allow", "default-allow", name


def surface_class(path: Path, decision: str) -> str:
    name = path.name
    if decision == "snapshot":
        return "sqlite_snapshot"
    if name.endswith(".jsonl"):
        return "transcript"
    if path.is_dir() and (path / ".git").exists():
        return "repo"
    if name.endswith((".json", ".jsonc", ".toml", ".md", ".yml", ".yaml")):
        return "agent_config"
    return "repo_or_file"


def repo_info(root: Path) -> tuple[str, str, str]:
    if not (root / ".git").exists():
        return "", "", ""

    branch = run_git(root, ["branch", "--show-current"])
    status = run_git(root, ["status", "--short"])
    remotes = run_git(root, ["remote", "-v"])
    dirty_rows = [row for row in status.splitlines() if row.strip()]
    remote_names = sorted({row.split()[0] for row in remotes.splitlines() if row.strip()})
    return branch, str(len(dirty_rows)), ",".join(remote_names)


def run_git(root: Path, args: list[str]) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), *args],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=20,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return result.stdout.strip() if result.returncode == 0 else ""


def iter_paths(source: Path) -> list[Path]:
    source = source.expanduser()
    if not source.exists():
        return [source]
    if source.is_file() or source.is_symlink():
        return [source]

    rows: list[Path] = [source]
    for current, dirs, files in os.walk(source, followlinks=False):
        current_path = Path(current)
        kept_dirs = []
        for dirname in dirs:
            path = current_path / dirname
            rows.append(path)
            decision, _, _ = classify(path, source)
            if decision == "allow":
                kept_dirs.append(dirname)
        dirs[:] = kept_dirs
        for filename in files:
            rows.append(current_path / filename)
    return rows


def build_manifest(sources: list[Path], *, hash_allowed: bool) -> list[ManifestRow]:
    rows: list[ManifestRow] = []
    for source in sources:
        root = source.expanduser()
        branch, dirty, remotes = repo_info(root) if root.exists() and root.is_dir() else ("", "", "")
        for path in iter_paths(root):
            if not path.exists() and not path.is_symlink():
                rows.append(
                    ManifestRow(
                        source_path=str(path),
                        surface_class="missing",
                        decision="deny",
                        reason="missing",
                        size_bytes=0,
                        mtime_utc="",
                    )
                )
                continue

            decision, reason, _ = classify(path, root)
            digest = ""
            if hash_allowed and decision == "allow" and path.is_file() and not path.is_symlink():
                digest = sha256_file(path)
            rows.append(
                ManifestRow(
                    source_path=str(path),
                    surface_class=surface_class(path, decision),
                    decision=decision,
                    reason=reason,
                    size_bytes=file_size(path),
                    mtime_utc=utc_mtime(path),
                    hash_sha256=digest,
                    repo_branch=branch,
                    repo_dirty_summary=dirty,
                    remote_summary=remotes,
                    snapshot_source=str(path) if decision == "snapshot" else "",
                )
            )
    return rows


def write_outputs(rows: list[ManifestRow], out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    with (out_dir / "manifest.jsonl").open("w") as f:
        for row in rows:
            f.write(json.dumps(asdict(row), sort_keys=True) + "\n")

    summary = {
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "rows": len(rows),
        "decisions": {},
        "reasons": {},
    }
    for row in rows:
        summary["decisions"][row.decision] = summary["decisions"].get(row.decision, 0) + 1
        summary["reasons"][row.reason] = summary["reasons"].get(row.reason, 0) + 1
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")


def main() -> None:
    args = parse_args()
    rows = build_manifest(args.sources, hash_allowed=args.hash_allowed)
    write_outputs(rows, args.out_dir)


if __name__ == "__main__":
    main()
