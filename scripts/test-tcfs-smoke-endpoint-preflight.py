#!/usr/bin/env python3
"""Regression tests for tcfs-smoke-endpoint-preflight.py."""

from __future__ import annotations

import pathlib
import subprocess
import sys


REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "tcfs-smoke-endpoint-preflight.py"


def run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args, "--skip-connect"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def assert_ok(*args: str, contains: str) -> None:
    proc = run(*args)
    combined = proc.stdout + proc.stderr
    if proc.returncode != 0:
        raise AssertionError(f"expected success, got {proc.returncode}\n{combined}")
    if contains not in combined:
        raise AssertionError(f"expected {contains!r} in output\n{combined}")


def assert_fails(*args: str, contains: str) -> None:
    proc = run(*args)
    combined = proc.stdout + proc.stderr
    if proc.returncode == 0:
        raise AssertionError(f"expected failure\n{combined}")
    if contains not in combined:
        raise AssertionError(f"expected {contains!r} in output\n{combined}")


def main() -> int:
    hosted = ("--runner-label", "ubuntu-24.04", "--platform", "linux")
    private = ("--runner-label", "petting-zoo-linux", "--platform", "linux")

    assert_fails(
        "--endpoint",
        "http://storage.example.com:8333",
        *hosted,
        contains="requires TCFS_SMOKE_S3_ENDPOINT to be an HTTPS URL",
    )
    assert_fails(
        "--endpoint",
        "https://seaweedfs-tcfs:8333",
        *hosted,
        contains="bare hostname",
    )
    assert_fails(
        "--endpoint",
        "https://10.0.0.10:8333",
        *hosted,
        contains="not globally routable",
    )
    assert_fails(
        "--endpoint",
        "https://seaweedfs-tcfs.ts.net:8333",
        *hosted,
        contains="tailnet or local DNS",
    )
    assert_ok(
        "--endpoint",
        "https://storage.example.com:8333",
        *hosted,
        contains="storage endpoint posture preflight passed",
    )
    assert_ok(
        "--endpoint",
        "http://seaweedfs-tcfs:8333",
        *private,
        contains="Self-hosted/private runner label",
    )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
