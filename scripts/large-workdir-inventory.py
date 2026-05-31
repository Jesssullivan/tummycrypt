#!/usr/bin/env python3
"""Create a read-only inventory packet for a candidate TCFS large workdir."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import stat
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path


@dataclass
class Inventory:
    root: str
    generated_at: str
    total_bytes: int = 0
    entries: int = 0
    regular_files: int = 0
    directories: int = 1
    symlinks: int = 0
    hidden_directories: int = 0
    special_files: int = 0
    unsupported_entries: list[str] = field(default_factory=list)
    xattr_probe: str = "checked"
    xattr_entries_with_attrs: int = 0
    xattr_errors: int = 0
    scan_errors: int = 0
    git_present: bool = False
    git_dirty: bool | None = None
    git_status_entries: int = 0
    git_status_error: str | None = None
    recommendation: str = "shadow_pilot_ready"
    recommendation_reason: str = "no blocking filesystem shape found"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Create a read-only inventory packet for a candidate TCFS workdir."
    )
    parser.add_argument("root", type=Path, help="Candidate workdir to inspect")
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="Directory for inventory.json, inventory.env, and summary.md",
    )
    parser.add_argument(
        "--skip-xattrs",
        action="store_true",
        help="Skip per-entry xattr probing for faster scans",
    )
    parser.add_argument(
        "--max-unsupported",
        type=int,
        default=100,
        help="Maximum unsupported entry paths to retain in inventory.json",
    )
    return parser.parse_args()


def list_xattrs(path: Path) -> list[str]:
    if not hasattr(os, "listxattr"):
        raise AttributeError("os.listxattr unavailable on this platform")
    try:
        return os.listxattr(path, follow_symlinks=False)
    except TypeError:
        return os.listxattr(path)


def rel(root: Path, path: Path) -> str:
    try:
        value = path.relative_to(root)
    except ValueError:
        value = path
    text = str(value)
    return "." if text == "." else text


def scan(root: Path, *, skip_xattrs: bool, max_unsupported: int) -> Inventory:
    resolved = root.expanduser().resolve()
    if not resolved.exists():
        raise FileNotFoundError(f"candidate root does not exist: {resolved}")
    if not resolved.is_dir():
        raise NotADirectoryError(f"candidate root is not a directory: {resolved}")

    inventory = Inventory(
        root=str(resolved),
        generated_at=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    )
    inventory.git_present = (resolved / ".git").exists()
    probe_xattrs = not skip_xattrs
    if skip_xattrs:
        inventory.xattr_probe = "skipped"

    stack = [resolved]
    while stack:
        current = stack.pop()
        try:
            entries = list(os.scandir(current))
        except OSError as exc:
            inventory.scan_errors += 1
            if len(inventory.unsupported_entries) < max_unsupported:
                inventory.unsupported_entries.append(f"{rel(resolved, current)}: scandir {exc}")
            continue

        for entry in entries:
            path = Path(entry.path)
            inventory.entries += 1
            try:
                mode = entry.stat(follow_symlinks=False).st_mode
            except OSError as exc:
                inventory.scan_errors += 1
                if len(inventory.unsupported_entries) < max_unsupported:
                    inventory.unsupported_entries.append(f"{rel(resolved, path)}: stat {exc}")
                continue

            if probe_xattrs:
                try:
                    if list_xattrs(path):
                        inventory.xattr_entries_with_attrs += 1
                except AttributeError:
                    inventory.xattr_probe = "unsupported"
                    probe_xattrs = False
                except OSError:
                    inventory.xattr_errors += 1

            if stat.S_ISDIR(mode):
                inventory.directories += 1
                if path.name.startswith("."):
                    inventory.hidden_directories += 1
                stack.append(path)
            elif stat.S_ISREG(mode):
                inventory.regular_files += 1
                inventory.total_bytes += entry.stat(follow_symlinks=False).st_size
            elif stat.S_ISLNK(mode):
                inventory.symlinks += 1
            else:
                inventory.special_files += 1
                if len(inventory.unsupported_entries) < max_unsupported:
                    inventory.unsupported_entries.append(rel(resolved, path))

    classify_git(resolved, inventory)
    classify_recommendation(inventory)
    return inventory


def classify_git(root: Path, inventory: Inventory) -> None:
    if not inventory.git_present:
        return
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "status", "--porcelain=v1", "--untracked-files=normal"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        inventory.git_dirty = None
        inventory.git_status_error = str(exc)
        return

    if result.returncode != 0:
        inventory.git_dirty = None
        inventory.git_status_error = result.stderr.strip() or f"git exited {result.returncode}"
        return

    lines = [line for line in result.stdout.splitlines() if line.strip()]
    inventory.git_status_entries = len(lines)
    inventory.git_dirty = bool(lines)


def classify_recommendation(inventory: Inventory) -> None:
    if inventory.special_files:
        inventory.recommendation = "blocked_special_files"
        inventory.recommendation_reason = (
            f"{inventory.special_files} special file(s) require explicit support or exclusion"
        )
    elif inventory.git_dirty:
        inventory.recommendation = "shadow_pilot_only_dirty_git"
        inventory.recommendation_reason = (
            f"git worktree has {inventory.git_status_entries} dirty/untracked entry rows"
        )
    else:
        inventory.recommendation = "shadow_pilot_ready"
        inventory.recommendation_reason = "no blocking filesystem shape found"


def write_outputs(inventory: Inventory, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    data = asdict(inventory)
    (out_dir / "inventory.json").write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    (out_dir / "inventory.env").write_text(render_env(data))
    (out_dir / "summary.md").write_text(render_summary(inventory))


def render_env(data: dict[str, object]) -> str:
    rows = []
    for key in sorted(data):
        value = data[key]
        if isinstance(value, (list, dict)):
            encoded = json.dumps(value, sort_keys=True)
        elif value is None:
            encoded = ""
        else:
            encoded = str(value)
        rows.append(f"{key}={shlex.quote(encoded)}")
    return "\n".join(rows) + "\n"


def render_summary(inventory: Inventory) -> str:
    return f"""# TCFS Large Workdir Inventory

- root: `{inventory.root}`
- generated_at: `{inventory.generated_at}`
- recommendation: `{inventory.recommendation}`
- reason: {inventory.recommendation_reason}

## Shape

| Metric | Value |
| --- | ---: |
| total bytes | {inventory.total_bytes} |
| entries | {inventory.entries} |
| regular files | {inventory.regular_files} |
| directories | {inventory.directories} |
| symlinks | {inventory.symlinks} |
| hidden directories | {inventory.hidden_directories} |
| special files | {inventory.special_files} |
| xattr entries with attrs | {inventory.xattr_entries_with_attrs} |
| xattr errors | {inventory.xattr_errors} |
| scan errors | {inventory.scan_errors} |

## Git

- git_present: `{str(inventory.git_present).lower()}`
- git_dirty: `{inventory.git_dirty}`
- git_status_entries: `{inventory.git_status_entries}`
- git_status_error: `{inventory.git_status_error or ""}`

## Claim Boundary

This packet is read-only inventory. It does not prove sync, restore, selective
sync, broad `~/git`, `/tmp`, or home-directory ownership.
"""


def main() -> int:
    args = parse_args()
    root = args.root
    out_dir = args.out_dir or Path.cwd() / "large-workdir-inventory"
    try:
        inventory = scan(
            root,
            skip_xattrs=args.skip_xattrs,
            max_unsupported=args.max_unsupported,
        )
        write_outputs(inventory, out_dir)
    except Exception as exc:
        print(f"large workdir inventory failed: {exc}", file=sys.stderr)
        return 1

    print(f"inventory: {out_dir / 'inventory.json'}")
    print(f"summary:   {out_dir / 'summary.md'}")
    print(f"result:    {inventory.recommendation}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
