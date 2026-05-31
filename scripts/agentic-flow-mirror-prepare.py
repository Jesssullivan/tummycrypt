#!/usr/bin/env python3
"""Prepare a local TCFS mirror staging directory from an agentic-flow manifest."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
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
    ".claude/worktrees",
    ".claude/backups",
    ".codex/cache",
    ".codex/log",
    ".codex/plugins/cache",
    ".codex/tmp",
    ".crush/logs",
    ".claude/plugins/cache",
    "cache",
    "backups",
    "log",
    "logs",
    "node_modules",
    "nix/secrets",
    "plugins/cache",
    "sops-nix",
    "target",
    "tmp",
    "worktrees",
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


@dataclass
class SnapshotResult:
    source_path: str
    snapshot_path: str
    status: str
    integrity_check: str
    journal_mode: str
    error: str = ""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Prepare a local, TCFS-ready staging tree from a manifest."
    )
    parser.add_argument("--manifest", type=Path, required=True, help="manifest.jsonl to consume")
    parser.add_argument("--stage-dir", type=Path, required=True, help="staging output directory")
    parser.add_argument(
        "--copy-allowed",
        action="store_true",
        help="Copy allowed non-transcript files and create allowed directories",
    )
    parser.add_argument(
        "--copy-transcripts",
        action="store_true",
        help="Also copy allowed .jsonl transcript rows. Requires --copy-allowed",
    )
    parser.add_argument(
        "--snapshot-sqlite",
        action="store_true",
        help="Snapshot rows marked decision=snapshot with sqlite3 .backup",
    )
    parser.add_argument(
        "--sqlite-timeout-secs",
        type=int,
        default=600,
        help="Timeout per sqlite3 backup/integrity command",
    )
    return parser.parse_args()


def is_dotenv(name: str) -> bool:
    return (
        name == ".env"
        or name == ".envrc"
        or name.startswith(".env.")
        or name.endswith(".env")
        or ".env." in name
    )


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


def load_rows(manifest: Path) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    with manifest.open() as f:
        for line_no, line in enumerate(f, start=1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{manifest}:{line_no}: invalid JSON: {exc}") from exc
            if not isinstance(row, dict):
                raise SystemExit(f"{manifest}:{line_no}: manifest row is not an object")
            rows.append(row)
    return rows


def source_path(row: dict[str, object]) -> Path:
    raw = row.get("source_path")
    if not isinstance(raw, str) or not raw:
        raise ValueError("row has no source_path")
    return Path(raw)


def path_under(stage_dir: Path, bucket: str, source: Path) -> Path:
    parts = source.parts[1:] if source.is_absolute() else source.parts
    if any(part in {"", ".", ".."} for part in parts):
        raise ValueError(f"unsafe source path for staging: {source}")
    if not parts:
        raise ValueError(f"source path has no stable staging components: {source}")
    return stage_dir / bucket / Path(*parts)


def is_transcript(row: dict[str, object], source: Path) -> bool:
    return row.get("surface_class") == "transcript" or source.name.endswith(".jsonl")


def unsafe_allow_reason(source: Path) -> str:
    denied_dir = has_component(source, DENY_DIRS)
    if denied_dir:
        return f"denied component {denied_dir}"
    name = source.name
    if name in DENY_FILES or is_dotenv(name):
        return f"credential/env name {name}"
    if name.endswith(DB_SUFFIXES) or name.endswith(DB_SIDECAR_SUFFIXES):
        return f"live database shape {name}"
    if name.endswith(".log"):
        return f"runtime log {name}"
    return ""


def validate_manifest(rows: list[dict[str, object]]) -> list[str]:
    errors: list[str] = []
    for idx, row in enumerate(rows, start=1):
        try:
            source = source_path(row)
        except ValueError as exc:
            errors.append(f"row {idx}: {exc}")
            continue
        decision = row.get("decision")
        if decision == "allow":
            reason = unsafe_allow_reason(source)
            if reason:
                errors.append(f"row {idx}: unsafe allow for {source}: {reason}")
            if source.is_symlink():
                errors.append(f"row {idx}: unsafe allow for symlink {source}")
        elif decision not in {"deny", "snapshot"}:
            errors.append(f"row {idx}: unknown decision {decision!r} for {source}")
    return errors


def decode_timeout(value: bytes | str | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


def run_sqlite(command: list[str], timeout_secs: int) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            ["sqlite3", *command],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_secs,
        )
    except subprocess.TimeoutExpired as exc:
        return subprocess.CompletedProcess(
            ["sqlite3", *command],
            124,
            stdout=decode_timeout(exc.stdout),
            stderr=f"sqlite3 timed out after {timeout_secs}s",
        )


def sqlite_snapshot(source: Path, dest: Path, timeout_secs: int) -> SnapshotResult:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists():
        dest.unlink()

    backup = run_sqlite([str(source), f".backup {dest}"], timeout_secs)
    if backup.returncode != 0:
        if dest.exists():
            dest.unlink()
        return SnapshotResult(
            source_path=str(source),
            snapshot_path=str(dest),
            status="failed",
            integrity_check="",
            journal_mode="",
            error=backup.stderr.strip() or backup.stdout.strip(),
        )

    integrity = run_sqlite([str(dest), "PRAGMA integrity_check;"], timeout_secs)
    integrity_text = integrity.stdout.strip()
    journal = run_sqlite([str(dest), "PRAGMA journal_mode;"], timeout_secs)
    journal_text = journal.stdout.strip()
    status = "ok" if integrity.returncode == 0 and integrity_text == "ok" else "failed"
    return SnapshotResult(
        source_path=str(source),
        snapshot_path=str(dest),
        status=status,
        integrity_check=integrity_text,
        journal_mode=journal_text,
        error=integrity.stderr.strip() if status != "ok" else "",
    )


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def prepare(rows: list[dict[str, object]], args: argparse.Namespace) -> int:
    stage_dir = args.stage_dir
    stage_dir.mkdir(parents=True, exist_ok=True)

    summary: dict[str, object] = {
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "manifest": str(args.manifest),
        "stage_dir": str(stage_dir),
        "copy_allowed_requested": bool(args.copy_allowed),
        "copy_transcripts_requested": bool(args.copy_transcripts),
        "snapshot_sqlite_requested": bool(args.snapshot_sqlite),
        "rows": len(rows),
        "allow_rows": 0,
        "deny_rows": 0,
        "snapshot_rows": 0,
        "created_dirs": 0,
        "copied_files": 0,
        "skipped_transcripts": 0,
        "snapshots_ok": 0,
        "snapshots_failed": 0,
        "errors": [],
    }
    snapshot_results: list[SnapshotResult] = []

    for row in rows:
        source = source_path(row)
        decision = row.get("decision")
        if decision == "deny":
            summary["deny_rows"] = int(summary["deny_rows"]) + 1
            continue

        if decision == "snapshot":
            summary["snapshot_rows"] = int(summary["snapshot_rows"]) + 1
            if not args.snapshot_sqlite:
                continue
            dest = path_under(stage_dir, "sqlite", source).with_suffix(
                source.suffix + ".snapshot.sqlite"
            )
            result = sqlite_snapshot(source, dest, int(args.sqlite_timeout_secs))
            snapshot_results.append(result)
            key = "snapshots_ok" if result.status == "ok" else "snapshots_failed"
            summary[key] = int(summary[key]) + 1
            if result.status != "ok":
                cast_errors = summary["errors"]
                assert isinstance(cast_errors, list)
                cast_errors.append(f"snapshot failed for {source}: {result.error}")
            continue

        summary["allow_rows"] = int(summary["allow_rows"]) + 1
        if not args.copy_allowed:
            continue
        if is_transcript(row, source) and not args.copy_transcripts:
            summary["skipped_transcripts"] = int(summary["skipped_transcripts"]) + 1
            continue

        dest = path_under(stage_dir, "files", source)
        if source.is_dir():
            dest.mkdir(parents=True, exist_ok=True)
            summary["created_dirs"] = int(summary["created_dirs"]) + 1
        elif source.is_file():
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest)
            summary["copied_files"] = int(summary["copied_files"]) + 1
        else:
            cast_errors = summary["errors"]
            assert isinstance(cast_errors, list)
            cast_errors.append(f"allowed source is not a regular file or directory: {source}")

    write_json(stage_dir / "prepare-summary.json", summary)
    with (stage_dir / "snapshot-results.jsonl").open("w") as f:
        for result in snapshot_results:
            f.write(json.dumps(asdict(result), sort_keys=True) + "\n")

    errors = summary["errors"]
    assert isinstance(errors, list)
    return 1 if errors else 0


def main() -> None:
    args = parse_args()
    if args.copy_transcripts and not args.copy_allowed:
        raise SystemExit("--copy-transcripts requires --copy-allowed")
    if args.snapshot_sqlite and shutil.which("sqlite3") is None:
        raise SystemExit("sqlite3 is required for --snapshot-sqlite")

    rows = load_rows(args.manifest)
    validation_errors = validate_manifest(rows)
    if validation_errors:
        for error in validation_errors:
            print(error, file=sys.stderr)
        raise SystemExit(1)
    raise SystemExit(prepare(rows, args))


if __name__ == "__main__":
    main()
