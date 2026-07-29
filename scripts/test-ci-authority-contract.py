#!/usr/bin/env python3
"""Fail-closed source contract for TCFS first-party CI authority."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any
import unittest

sys.dont_write_bytecode = True

PENDING_GF_REVISION = "TIN_3127_SIGNED_MERGE_REVISION_PENDING"
GF_ACTION_ROOT = "tinyland-inc/GloriousFlywheel/.github/actions/"
ATTIC_PUBLIC_KEY = "main:eaUydxuDu7xBoy5cCo3MdknYAkVyTIASQ7DGuwxa+XA="
LOCAL_DEV_ATTIC_SUBSTITUTER = "https://nix-cache.tinyland.dev/main"
EXPECTED_SHA_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && "
    "github.event.pull_request.head.sha || github.sha }}"
)
SAME_REPOSITORY_OR_NON_PR_CONDITION = (
    "${{ github.event_name != 'pull_request' || "
    "github.event.pull_request.head.repo.full_name == github.repository }}"
)
DISABLED_JOB_CONDITION = "${{ github.repository == '__TIN_2538_DISABLED__' }}"
HOSTED_RUNNER_PATTERN = re.compile(
    r"(?:ubuntu|macos|windows)-(?:latest|[0-9][A-Za-z0-9.-]*)",
    re.IGNORECASE,
)
FORBIDDEN_CACHE_OR_BOOTSTRAP_TOKENS = (
    "actions/cache",
    "Swatinem/rust-cache",
    "type=gha",
    "ACTIONS_CACHE_URL",
    "ACTIONS_RESULTS_URL",
    "cachix/install-nix-action",
    "DeterminateSystems/flakehub-cache-action",
    "bazel-contrib/setup-bazel",
)
SEAWEED_IMAGE = (
    "chrislusf/seaweedfs:4.40@"
    "sha256:52194fba4fecd0083c842158b3a902ba6e04a63619b2b0efcd08007bdb6a4602"
)
NATS_IMAGE = (
    "nats:2.10.29-alpine3.22@"
    "sha256:b83efabe3e7def1e0a4a31ec6e078999bb17c80363f881df35edc70fcb6bb927"
)
PUBLIC_READ_SITES = {
    (".github/workflows/ci.yml", "linux-source"): "tcfs-linux-source",
    (".github/workflows/ci.yml", "windows-cross"): "tcfs-windows-cross",
    (".github/workflows/nix-ci.yml", "nix-linux"): "tcfs-nix-linux",
    (".github/workflows/ci-live-storage.yml", "fleet-live"): ("tcfs-live-storage"),
}
WORKLOAD_MARKERS = {
    (".github/workflows/ci.yml", "linux-source"): (
        "nix develop .#default --command bash -euo pipefail",
        "cargo test --workspace --locked",
        "cargo deny check",
        "gitleaks git --config .gitleaks.toml --redact",
    ),
    (".github/workflows/ci.yml", "windows-cross"): (
        "nix develop .#default --command",
        "cargo check -p tcfs-cloudfilter",
        "--target x86_64-pc-windows-gnu",
    ),
    (".github/workflows/ci-live-storage.yml", "fleet-live"): (
        "docker compose -f docker-compose.yml",
        "nix develop .#default --command",
        "s3api head-bucket",
        "cargo test -p tcfs-e2e --test fleet_live --locked",
    ),
    (".github/workflows/nix-ci.yml", "nix-linux"): (
        "nix flake check",
        'nix build --fallback ".#${package}"',
    ),
}
LIVE_STORAGE_PATHS = [
    "crates/**",
    "tests/**",
    "Cargo.toml",
    "Cargo.lock",
    "docker-compose.yml",
    "config/**",
    ".github/workflows/ci-live-storage.yml",
    "scripts/test-ci-authority-contract.py",
    "config/ci-authority-policy.json",
]
PROTECTED_TRIGGER_CONTRACTS: dict[str, dict[str, Any]] = {
    ".github/workflows/ci.yml": {
        "push": {"branches": ["main", "dev", "sid/**", "1-*"]},
        "pull_request": None,
    },
    ".github/workflows/ci-live-storage.yml": {
        "pull_request": {"paths": LIVE_STORAGE_PATHS},
        "push": {
            "branches": ["main"],
            "paths": LIVE_STORAGE_PATHS,
        },
        "workflow_dispatch": None,
    },
    ".github/workflows/nix-ci.yml": {
        "push": {"branches": ["main", "dev"]},
        "pull_request": None,
        "workflow_dispatch": None,
    },
}


class ContractError(ValueError):
    """The checked-in CI authority contract is unsafe or incomplete."""


class PromotionHold(ContractError):
    """The source shape is reviewable but cannot be promoted."""


def find_repo_root() -> Path:
    for candidate in [Path.cwd(), Path(__file__).resolve()]:
        for parent in [candidate, *candidate.parents]:
            if (parent / "config/ci-authority-policy.json").is_file():
                return parent
    raise ContractError("cannot locate repository root")


def load_policy(root: Path) -> dict[str, Any]:
    loaded = json.loads(
        (root / "config/ci-authority-policy.json").read_text(encoding="utf-8"),
        object_pairs_hook=reject_duplicate_pairs,
    )
    if not isinstance(loaded, dict):
        raise ContractError("CI authority policy must be a JSON object")
    return loaded


def reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    loaded: dict[str, Any] = {}
    for key, value in pairs:
        if key in loaded:
            raise ContractError(f"yq JSON contains a duplicate mapping key: {key!r}")
        loaded[key] = value
    return loaded


def yq_json(path: Path, expression: str = ".") -> Any:
    try:
        completed = subprocess.run(
            ["yq", "--output-format=json", "--no-colors", expression, str(path)],
            check=True,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as error:
        raise ContractError("mikefarah yq v4 is required") from error
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip()
        raise ContractError(f"{path} is not valid YAML: {detail}") from error

    try:
        return json.loads(
            completed.stdout,
            object_pairs_hook=reject_duplicate_pairs,
        )
    except json.JSONDecodeError as error:
        raise ContractError(
            f"{path} did not produce one strict JSON document: {error}"
        ) from error


def load_workflow(path: Path) -> tuple[dict[str, Any], str]:
    source = path.read_text(encoding="utf-8")
    document = yq_json(path)
    if not isinstance(document, dict):
        raise ContractError(f"{path} must contain one top-level mapping")

    references = yq_json(
        path,
        ('[.. | select(anchor != "") | {"anchor": anchor, "path": path}]'),
    )
    if references:
        raise ContractError(f"{path} must not use YAML anchors, aliases, or merges")

    styled_keys = yq_json(
        path,
        (
            '[.. | select(tag == "!!map") | to_entries | .[] | '
            'select((.key | style) != "") | '
            '{"key": (.key | tostring), "style": (.key | style)}]'
        ),
    )
    if styled_keys:
        raise ContractError(
            f"{path} must use canonical unquoted mapping keys: {styled_keys!r}"
        )

    return document, source


def semantic_topology(
    value: Any,
    *,
    gf_revision: str,
    key: str | None = None,
) -> Any:
    expected_nix_job = f"{GF_ACTION_ROOT}nix-job@{gf_revision}"
    if key == "uses" and value == expected_nix_job:
        return f"{GF_ACTION_ROOT}nix-job@<GF_REVISION>"
    if isinstance(value, dict):
        return {
            item_key: semantic_topology(
                item_value,
                gf_revision=gf_revision,
                key=item_key,
            )
            for item_key, item_value in value.items()
        }
    if isinstance(value, list):
        return [semantic_topology(item, gf_revision=gf_revision) for item in value]
    return value


def topology_sha256(document: dict[str, Any], *, gf_revision: str) -> str:
    topology = semantic_topology(document, gf_revision=gf_revision)
    encoded = json.dumps(
        topology,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def validate_topology(
    document: dict[str, Any],
    contract: dict[str, Any],
    *,
    gf_revision: str,
) -> None:
    expected = contract.get("topology_sha256")
    if not isinstance(expected, str) or re.fullmatch(r"[0-9a-f]{64}", expected) is None:
        raise ContractError(f"{contract['path']} lacks a reviewed topology digest")
    actual = topology_sha256(document, gf_revision=gf_revision)
    if actual != expected:
        raise ContractError(
            f"{contract['path']} workflow topology drifted "
            f"(actual={actual}, expected={expected})"
        )


def workflow_jobs(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    jobs = document.get("jobs")
    if not isinstance(jobs, dict) or not jobs:
        raise ContractError("workflow jobs must be a non-empty mapping")
    for job_name, job in jobs.items():
        if not isinstance(job_name, str) or not isinstance(job, dict):
            raise ContractError("every workflow job must be a named mapping")
    return jobs


def workflow_steps(job_name: str, job: dict[str, Any]) -> list[dict[str, Any]]:
    steps = job.get("steps")
    if not isinstance(steps, list) or not steps:
        raise ContractError(f"{job_name} must contain a non-empty steps list")
    for step in steps:
        if not isinstance(step, dict):
            raise ContractError(f"{job_name} contains a non-mapping step")
    return steps


def workflow_triggers(document: dict[str, Any]) -> list[str]:
    triggers = document.get("on")
    if not isinstance(triggers, dict) or not triggers:
        raise ContractError("workflow on must be a non-empty mapping")
    if not all(isinstance(trigger, str) for trigger in triggers):
        raise ContractError("every workflow trigger must be a string key")
    return list(triggers)


def validate_protected_triggers(
    document: dict[str, Any],
    contract: dict[str, Any],
) -> None:
    path = contract.get("path")
    if contract.get("pull_request_all_bases") is not True:
        raise ContractError(f"{path} lacks the reviewed all-PR-base contract")

    triggers = document.get("on")
    expected = PROTECTED_TRIGGER_CONTRACTS.get(path)
    if expected is None:
        raise ContractError(f"{path} lacks a source-owned protected trigger contract")
    if triggers != expected:
        raise ContractError(
            f"{path} protected trigger contract drifted "
            f"(actual={triggers!r}, expected={expected!r})"
        )


def validate_permissions(document: dict[str, Any]) -> None:
    if document.get("permissions") != {"contents": "read"}:
        raise ContractError(
            "protected workflow permissions must be exactly contents: read"
        )


def all_action_steps(
    jobs: dict[str, dict[str, Any]],
) -> list[tuple[str, dict[str, Any]]]:
    found: list[tuple[str, dict[str, Any]]] = []
    for job_name, job in jobs.items():
        if "uses" in job:
            raise ContractError(f"job-level reusable workflow is forbidden: {job_name}")
        for step in workflow_steps(job_name, job):
            if "uses" in step:
                found.append((job_name, step))
    return found


def validate_action_refs(
    jobs: dict[str, dict[str, Any]],
    *,
    checkout_revision: str,
    gf_revision: str,
    front_door: str,
) -> None:
    allowed = {
        f"actions/checkout@{checkout_revision}",
        f"{GF_ACTION_ROOT}{front_door}@{gf_revision}",
    }
    action_steps = all_action_steps(jobs)
    if not action_steps:
        raise ContractError("protected workflow must use reviewed actions")
    for job_name, step in action_steps:
        action_ref = step.get("uses")
        if not isinstance(action_ref, str) or action_ref not in allowed:
            raise ContractError(
                f"{job_name} contains an unreviewed action reference: {action_ref!r}"
            )
        revision = action_ref.rsplit("@", 1)[-1]
        if action_ref.startswith("actions/checkout@"):
            if re.fullmatch(r"[0-9a-f]{40}", revision) is None:
                raise ContractError("checkout action revision must be immutable")
        elif (
            revision != PENDING_GF_REVISION
            and re.fullmatch(r"[0-9a-f]{40}", revision) is None
        ):
            raise ContractError("GloriousFlywheel action revision must be immutable")


def unique_step(
    job_name: str,
    steps: list[dict[str, Any]],
    *,
    name: str | None = None,
    uses: str | None = None,
) -> tuple[int, dict[str, Any]]:
    matches: list[tuple[int, dict[str, Any]]] = []
    for index, step in enumerate(steps):
        if name is not None and step.get("name") != name:
            continue
        if uses is not None and step.get("uses") != uses:
            continue
        matches.append((index, step))
    if len(matches) != 1:
        identity = name if name is not None else uses
        raise ContractError(f"{job_name} must contain exactly one step {identity!r}")
    return matches[0]


def validate_public_read_tuple(
    path: str,
    job_name: str,
    job: dict[str, Any],
    *,
    checkout_revision: str,
    gf_revision: str,
    expected_command_sha256: str,
) -> None:
    expected_action = f"{GF_ACTION_ROOT}nix-job@{gf_revision}"
    steps = workflow_steps(job_name, job)
    checkout_index, checkout = unique_step(
        job_name,
        steps,
        uses=f"actions/checkout@{checkout_revision}",
    )
    action_index, action = unique_step(
        job_name,
        steps,
        uses=expected_action,
    )
    nix_index, require_nix = unique_step(
        job_name,
        steps,
        name="Require the preinstalled GF Nix runtime",
    )
    if not checkout_index < nix_index < action_index:
        raise ContractError(
            f"{job_name} must verify preinstalled Nix before entering GF"
        )
    if require_nix.get("run") != "command -v nix":
        raise ContractError(f"{job_name} Nix preflight must fail closed")

    checkout_with = checkout.get("with")
    if not isinstance(checkout_with, dict):
        raise ContractError(f"{job_name} checkout must define a with mapping")
    if checkout_with != {
        "fetch-depth": 0,
        "ref": EXPECTED_SHA_EXPRESSION,
        "persist-credentials": False,
    }:
        raise ContractError(f"{job_name} checkout authority tuple drifted")

    unique_step(
        job_name,
        steps,
        name="Verify exact checked out revision",
    )
    revision_step = next(
        step
        for step in steps
        if step.get("name") == "Verify exact checked out revision"
    )
    if revision_step.get("env") != {"EXPECTED_SHA": EXPECTED_SHA_EXPRESSION}:
        raise ContractError(f"{job_name} revision identity drifted")
    if 'test "$(git rev-parse HEAD)" = "$EXPECTED_SHA"' not in str(
        revision_step.get("run", "")
    ):
        raise ContractError(f"{job_name} does not verify the checked-out revision")

    expected_site = PUBLIC_READ_SITES.get((path, job_name))
    if expected_site is None:
        raise ContractError(f"{path}:{job_name} has no reviewed public-read site")
    action_env = action.get("env")
    action_with = action.get("with")
    if not isinstance(action_env, dict) or not isinstance(action_with, dict):
        raise ContractError(f"{job_name} GF action must define env and with mappings")
    expected_action_env = {
        "GF_EXPECTED_RUNNER_ENVIRONMENT": "${{ runner.environment }}",
        "ATTIC_TOKEN": "",
    }
    if (path, job_name) == (".github/workflows/ci.yml", "linux-source"):
        expected_action_env["BASE_SHA"] = (
            "${{ github.event_name == 'pull_request' && "
            "github.event.pull_request.base.sha || '' }}"
        )
    if action_env != expected_action_env:
        raise ContractError(f"{job_name} GF action environment drifted")

    exact_public_values = {
        "attic-enabled": "true",
        "attic-public-key": ATTIC_PUBLIC_KEY,
        "attic-public-read-only": "true",
        "attic-public-read-site": expected_site,
        "push-cache": "false",
        "require-cache-push": "false",
    }
    for key, expected in exact_public_values.items():
        if action_with.get(key) != expected:
            raise ContractError(
                f"{job_name} must set public-read input {key}: {expected!r}"
            )
    for forbidden in ("attic-server", "attic-cache"):
        if forbidden in action_with:
            raise ContractError(f"{job_name} must not override {forbidden}")
    if set(action_with) != set(exact_public_values) | {"command"}:
        raise ContractError(f"{job_name} GF action inputs drifted")
    command = action_with.get("command")
    if not isinstance(command, str) or not command:
        raise ContractError(f"{job_name} GF command is missing")
    if hashlib.sha256(command.encode()).hexdigest() != expected_command_sha256:
        raise ContractError(f"{job_name} GF command body drifted")
    for required in (
        'test "${GF_EXPECTED_RUNNER_ENVIRONMENT:-}" = self-hosted',
        'test "${ATTIC_TOKEN:-}" = ""',
        'test -n "${ATTIC_SERVER:-}"',
        'test "${ATTIC_CACHE:-}" = main',
        'test -n "${ATTIC_PUBLIC_KEY:-}"',
        f'test "${{ATTIC_PUBLIC_KEY:-}}" = "{ATTIC_PUBLIC_KEY}"',
        'test -n "${BAZEL_REMOTE_CACHE:-}"',
        'test "${GF_BAZEL_SUBSTRATE_MODE:-}" = shared-cache-backed',
        'test "${NIX_USER_CONF_FILES:-}" = /dev/null',
        'test "${NETRC:-}" = /dev/null',
        'test "$(nix config show netrc-file)" = /dev/null',
        'grep -Fx -- "${ATTIC_SERVER%/}/${ATTIC_CACHE}"',
    ):
        if required not in command:
            raise ContractError(f"{job_name} lacks public-read check: {required}")
    lowered = command.lower()
    for forbidden in ("attic login", "attic push", "secrets.attic_token", "type=gha"):
        if forbidden in lowered:
            raise ContractError(
                f"{job_name} public-read command contains {forbidden!r}"
            )
    for marker in WORKLOAD_MARKERS[(path, job_name)]:
        if marker not in command:
            raise ContractError(f"{job_name} workload escaped the GF wrapper")

    trailing_steps = steps[action_index + 1 :]
    if path == ".github/workflows/ci-live-storage.yml":
        trailing_shape = [(step.get("name"), step.get("if")) for step in trailing_steps]
        if trailing_shape != [
            ("Print compose logs on failure", "failure()"),
            ("Tear down compose stack", "always()"),
        ]:
            raise ContractError(f"{job_name} has unaudited work after the GF wrapper")
        for step in trailing_steps:
            trailing_run = str(step.get("run", ""))
            if re.search(r"\b(?:nix|cargo)\b", trailing_run):
                raise ContractError(
                    f"{job_name} has Nix-dependent work after the GF wrapper"
                )
    elif trailing_steps:
        raise ContractError(f"{job_name} has work after the GF wrapper")


def validate_held_job(job_name: str, job: dict[str, Any], issue: str) -> None:
    if "uses" in job:
        raise ContractError(f"{job_name} held job must not delegate")
    steps = workflow_steps(job_name, job)
    if len(steps) != 1:
        raise ContractError(f"{job_name} must contain one fail-closed step")
    step = steps[0]
    name = step.get("name")
    run = step.get("run")
    if not isinstance(name, str) or not name.startswith("Hold until "):
        raise ContractError(f"{job_name} held step name drifted")
    if not isinstance(run, str) or issue not in run:
        raise ContractError(f"{job_name} must name its prerequisite {issue}")
    if not run.rstrip().endswith("exit 1"):
        raise ContractError(f"{job_name} must end at fail-closed exit 1")
    if re.search(r"(?m)^\s+exit 0\s*$|\|\|\s*true", run):
        raise ContractError(f"{job_name} contains a success bypass")


def validate_live_storage_images(source: str) -> None:
    image_values = re.findall(r"(?m)^\s+image:\s*(\S+)\s*$", source)
    expected = [SEAWEED_IMAGE] * 5 + [NATS_IMAGE]
    if sorted(image_values) != sorted(expected):
        raise ContractError(
            "live-storage service images must match the reviewed digests"
        )
    for image in image_values:
        if re.fullmatch(r"[^@\s]+@sha256:[0-9a-f]{64}", image) is None:
            raise ContractError(f"live-storage image is not digest-pinned: {image}")


def validate_protected_workflow(
    document: dict[str, Any],
    source: str,
    contract: dict[str, Any],
    policy: dict[str, Any],
) -> None:
    validate_topology(
        document,
        contract,
        gf_revision=policy["gloriousflywheel_revision"],
    )
    validate_protected_triggers(document, contract)
    validate_permissions(document)
    path = contract["path"]
    expected_jobs = contract["jobs"]
    held_jobs = contract["held_jobs"]
    command_sha256 = contract.get("command_sha256")
    front_door = contract["front_door"]
    if not isinstance(path, str) or not isinstance(expected_jobs, dict):
        raise ContractError("protected policy entry has an invalid shape")
    if not isinstance(held_jobs, dict) or front_door != "nix-job":
        raise ContractError("protected workflows must use the nix-job front door")
    productive_jobs = set(expected_jobs) - set(held_jobs)
    if (
        not isinstance(command_sha256, dict)
        or set(command_sha256) != productive_jobs
        or any(
            not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None
            for value in command_sha256.values()
        )
    ):
        raise ContractError("protected GF command digest inventory drifted")

    if HOSTED_RUNNER_PATTERN.search(source):
        raise ContractError("GitHub-hosted runner label is forbidden")
    if re.search(r"accept-flake-config", source, re.IGNORECASE):
        raise ContractError("protected proof must not accept checked-in flake config")
    normalized = re.sub(r"\s+", "", source.lower())
    if "type=gha" in normalized:
        raise ContractError("GitHub cache provider is forbidden")
    for token in FORBIDDEN_CACHE_OR_BOOTSTRAP_TOKENS:
        if token.lower() in source.lower():
            raise ContractError(f"forbidden cache/bootstrap path: {token}")

    if document.get("env", {}).get("ATTIC_TOKEN") != "":
        raise ContractError(
            "protected workflow must clear ambient Attic credentials globally"
        )
    if policy.get("credential_boundary") != {
        "attic": "public-read-only; ATTIC_TOKEN empty; inherited netrc disabled",
        "github": (
            "repository-scoped contents:read token may remain available to "
            "Nix source fetches"
        ),
        "claim": "not globally credential-free",
    }:
        raise ContractError("credential claim boundary drifted")
    for required_claim in (
        "Attic authentication",
        "contents:read GitHub token",
    ):
        if required_claim not in source:
            raise ContractError(
                f"protected workflow omits credential boundary: {required_claim}"
            )

    jobs = workflow_jobs(document)
    if set(jobs) != set(expected_jobs):
        raise ContractError(
            "protected job inventory drifted "
            f"(actual={sorted(jobs)}, expected={sorted(expected_jobs)})"
        )
    for job_name, expected_runner in expected_jobs.items():
        job = jobs[job_name]
        if job.get("runs-on") != expected_runner or not isinstance(
            job.get("runs-on"), str
        ):
            raise ContractError(
                f"{job_name} must use literal runner {expected_runner!r}"
            )
        if job.get("if") != SAME_REPOSITORY_OR_NON_PR_CONDITION:
            raise ContractError(
                f"{job_name} must use the exact same-repository fork guard"
            )
        if job_name in held_jobs:
            validate_held_job(job_name, job, held_jobs[job_name])
        else:
            if "uses" in job:
                raise ContractError(f"{job_name} must not delegate")
            validate_public_read_tuple(
                path,
                job_name,
                job,
                checkout_revision=policy["checkout_revision"],
                gf_revision=policy["gloriousflywheel_revision"],
                expected_command_sha256=command_sha256[job_name],
            )

    validate_action_refs(
        jobs,
        checkout_revision=policy["checkout_revision"],
        gf_revision=policy["gloriousflywheel_revision"],
        front_door=front_door,
    )

    conditions = [
        step["if"]
        for job_name, job in jobs.items()
        for step in workflow_steps(job_name, job)
        if "if" in step
    ]
    allowed_conditions = (
        ["failure()", "always()"]
        if path == ".github/workflows/ci-live-storage.yml"
        else []
    )
    if conditions != allowed_conditions:
        raise ContractError("protected proof contains an unaudited conditional")

    if path == ".github/workflows/ci-live-storage.yml":
        validate_live_storage_images(source)
        live_command = next(
            step["with"]["command"]
            for step in workflow_steps("fleet-live", jobs["fleet-live"])
            if step.get("uses", "").startswith(GF_ACTION_ROOT)
        )
        for required in (
            'test -n "${DOCKER_HOST:-}"',
            "docker version",
            "docker info",
        ):
            if required not in live_command:
                raise ContractError(f"live-storage lacks DinD check: {required}")
    if path == ".github/workflows/ci.yml" and (
        "--target x86_64-pc-windows-gnu" not in source
    ):
        raise ContractError("Windows source proof must use the reviewed cross target")


def validate_disabled_workflow(
    document: dict[str, Any],
    contract: dict[str, Any],
    *,
    gf_revision: str,
    protected_job_index: dict[tuple[str, str], str],
) -> None:
    validate_topology(document, contract, gf_revision=gf_revision)
    expected_triggers = contract["triggers"]
    actual_triggers = workflow_triggers(document)
    if actual_triggers != expected_triggers:
        raise ContractError(
            "disabled workflow trigger inventory drifted "
            f"(actual={actual_triggers}, expected={expected_triggers})"
        )
    jobs = workflow_jobs(document)
    parity = contract.get("job_parity")
    if not isinstance(parity, dict) or set(parity) != set(jobs):
        raise ContractError("disabled workflow job parity inventory drifted")
    for job_name, job in jobs.items():
        if job.get("if") != DISABLED_JOB_CONDITION:
            raise ContractError(
                f"disabled workflow job can allocate a runner: {job_name}"
            )
        claim = parity[job_name]
        expected_fields = {
            "disposition",
            "owner",
            "replacement",
            "blocked_by",
            "reason",
        }
        if not isinstance(claim, dict) or set(claim) != expected_fields:
            raise ContractError(f"{job_name} parity claim shape drifted")
        if claim["owner"] != contract["owner"]:
            raise ContractError(f"{job_name} parity owner drifted")
        if not isinstance(claim["reason"], str) or not claim["reason"].strip():
            raise ContractError(f"{job_name} parity reason is empty")

        disposition = claim["disposition"]
        replacement = claim["replacement"]
        blocked_by = claim["blocked_by"]
        if disposition == "held":
            if (
                replacement is not None
                or not isinstance(blocked_by, str)
                or re.fullmatch(r"TIN-[1-9][0-9]*", blocked_by) is None
            ):
                raise ContractError(f"{job_name} held parity claim is incomplete")
        elif disposition == "retired":
            if replacement is not None or blocked_by is not None:
                raise ContractError(f"{job_name} retired parity claim drifted")
        elif disposition == "migrated":
            if blocked_by is not None or not isinstance(replacement, dict):
                raise ContractError(f"{job_name} migrated parity claim is incomplete")
            if set(replacement) != {"path", "job", "runner_class"}:
                raise ContractError(f"{job_name} replacement shape drifted")
            target = (replacement["path"], replacement["job"])
            if protected_job_index.get(target) != replacement["runner_class"]:
                raise ContractError(f"{job_name} replacement is not protected")
        else:
            raise ContractError(f"{job_name} has unknown parity disposition")


def protected_job_index(policy: dict[str, Any]) -> dict[tuple[str, str], str]:
    return {
        (contract["path"], job_name): runner
        for contract in policy["protected_proof"]
        for job_name, runner in contract["jobs"].items()
        if job_name not in contract["held_jobs"]
    }


def validate_inventory(root: Path, policy: dict[str, Any]) -> None:
    protected = policy["protected_proof"]
    disabled = policy["disabled_legacy_workflows"]
    protected_paths = [entry["path"] for entry in protected]
    disabled_paths = [entry["path"] for entry in disabled]
    declared = protected_paths + disabled_paths
    if len(declared) != len(set(declared)):
        raise ContractError("workflow authority ledger has duplicate paths")
    actual = sorted(
        str(path.relative_to(root))
        for path in (root / ".github/workflows").iterdir()
        if path.suffix in {".yml", ".yaml"}
    )
    if sorted(declared) != actual:
        raise ContractError(
            "workflow authority ledger is not closed "
            f"(undeclared={sorted(set(actual) - set(declared))}, "
            f"missing={sorted(set(declared) - set(actual))})"
        )

    for entry in disabled:
        if entry.get("owner") != policy["issue"]:
            raise ContractError(
                "every disabled workflow must retain explicit issue ownership"
            )
        document, _ = load_workflow(root / entry["path"])
        validate_disabled_workflow(
            document,
            entry,
            gf_revision=policy["gloriousflywheel_revision"],
            protected_job_index=protected_job_index(policy),
        )


def validate_actionlint_labels(root: Path, policy: dict[str, Any]) -> None:
    expected = sorted(
        {
            runner
            for contract in policy["protected_proof"]
            for runner in contract["jobs"].values()
        }
    )
    config = (root / ".github/actionlint.yaml").read_text(encoding="utf-8")
    rendered = "self-hosted-runner:\n  labels:\n" + "".join(
        f"    - {label}\n" for label in expected
    )
    if config != rendered:
        raise ContractError(
            "actionlint labels must exactly match protected capability lanes"
        )


def validate_attic_public_read(root: Path, policy: dict[str, Any]) -> None:
    expected_policy = {
        "public_key": ATTIC_PUBLIC_KEY,
        "local_dev_substituter": LOCAL_DEV_ATTIC_SUBSTITUTER,
    }
    if policy.get("attic_public_read") != expected_policy:
        raise ContractError("Attic public-read policy drifted")

    expected_block = (
        "  nixConfig = {\n"
        "    extra-substituters = [\n"
        f'      "{LOCAL_DEV_ATTIC_SUBSTITUTER}"\n'
        "    ];\n"
        "    extra-trusted-public-keys = [\n"
        f'      "{ATTIC_PUBLIC_KEY}"\n'
        "    ];\n"
        "  };"
    )
    source = (root / "flake.nix").read_text(encoding="utf-8")
    if source.count(expected_block) != 1:
        raise ContractError("flake.nix public Attic tuple drifted")
    for token in (
        "nixConfig = {",
        "extra-substituters = [",
        "extra-trusted-public-keys = [",
        LOCAL_DEV_ATTIC_SUBSTITUTER,
        ATTIC_PUBLIC_KEY,
    ):
        if source.count(token) != 1:
            raise ContractError(f"flake.nix contains an ambiguous Attic token: {token}")


def validate_current_tree(root: Path, policy: dict[str, Any]) -> None:
    if policy.get("version") != 2 or policy.get("issue") != "TIN-2538":
        raise ContractError("CI authority policy identity or version drifted")
    live_hold = policy.get("live_proof_hold")
    if (
        not isinstance(live_hold, dict)
        or live_hold.get("issue") != "TIN-3120"
        or "GitHub-hosted capacity is never a fallback"
        not in str(live_hold.get("reason", ""))
    ):
        raise ContractError("TIN-3120 live-proof hold or no-fallback claim drifted")
    if policy.get("blocked_prerequisites") != {
        "multi_capability_personal_owner_binding": "TIN-3120",
        "native_darwin_tcfs_integration": "TIN-2538",
        "native_windows_runtime_and_installer": "TIN-1569",
    }:
        raise ContractError("CI authority prerequisite ledger drifted")
    if policy.get("completed_provenance") != {
        "native_darwin_worker_commissioned": "TIN-2998"
    }:
        raise ContractError("CI authority completed provenance drifted")
    validate_inventory(root, policy)
    validate_actionlint_labels(root, policy)
    validate_attic_public_read(root, policy)
    for contract in policy["protected_proof"]:
        document, source = load_workflow(root / contract["path"])
        validate_protected_workflow(document, source, contract, policy)


def validate_promotable_revision(policy: dict[str, Any]) -> None:
    revision = policy.get("gloriousflywheel_revision")
    hold = policy.get("promotion_hold")
    if revision == PENDING_GF_REVISION:
        if not isinstance(hold, dict) or hold.get("issue") != "TIN-3127":
            raise ContractError("pending GF revision lacks its exact TIN-3127 hold")
        raise PromotionHold(
            "TIN-3127 signed merge revision is unavailable; "
            "the protected workflows are intentionally non-promotable"
        )
    if not isinstance(revision, str) or re.fullmatch(r"[0-9a-f]{40}", revision) is None:
        raise ContractError("GloriousFlywheel revision must be a full commit SHA")
    if hold is not None:
        raise ContractError("resolved GloriousFlywheel revision retains a stale hold")


class CiAuthorityContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = find_repo_root()
        cls.policy = load_policy(cls.root)
        cls.contracts = {
            entry["path"]: entry for entry in cls.policy["protected_proof"]
        }
        cls.sources = {
            path: (cls.root / path).read_text(encoding="utf-8")
            for path in cls.contracts
        }

    def assert_protected_rejected(self, path: str, unsafe: str) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            candidate = Path(temporary) / "workflow.yml"
            candidate.write_text(unsafe, encoding="utf-8")
            with self.assertRaises(ContractError):
                document, source = load_workflow(candidate)
                validate_protected_workflow(
                    document,
                    source,
                    self.contracts[path],
                    self.policy,
                )

    def test_current_tree_is_closed_and_fail_closed(self) -> None:
        validate_current_tree(self.root, self.policy)

    def test_revision_promotion_state_is_explicit(self) -> None:
        if self.policy["gloriousflywheel_revision"] == PENDING_GF_REVISION:
            with self.assertRaisesRegex(PromotionHold, "TIN-3127"):
                validate_promotable_revision(self.policy)
        else:
            validate_promotable_revision(self.policy)

    def test_quoted_job_and_trigger_keys_are_rejected(self) -> None:
        path = ".github/workflows/ci.yml"
        source = self.sources[path]
        variants = [
            source.replace("  linux-source:\n", '  "linux-source":\n', 1),
            source.replace(
                "  linux-source:\n",
                '  "linux\\u002dsource":\n',
                1,
            ),
            source.replace("  pull_request:\n", '  "pull_request":\n', 1),
        ]
        for unsafe in variants:
            self.assert_protected_rejected(path, unsafe)

    def test_protected_trigger_contract_rejects_narrowing_and_expansion(self) -> None:
        for path in sorted(self.contracts):
            document, parsed_source = load_workflow(self.root / path)
            pull_request_variants = [
                {"branches": ["main"]},
                {"branches-ignore": ["dev"]},
                {"types": ["closed"]},
                {"paths": ["README.md"]},
                {"paths-ignore": ["**"]},
            ]
            unsafe_documents: list[dict[str, Any]] = []
            for pull_request in pull_request_variants:
                unsafe = copy.deepcopy(document)
                unsafe["on"]["pull_request"] = pull_request
                unsafe_documents.append(unsafe)
            for trigger, value in (
                ("pull_request_target", None),
                ("schedule", [{"cron": "0 0 * * *"}]),
            ):
                unsafe = copy.deepcopy(document)
                unsafe["on"][trigger] = value
                unsafe_documents.append(unsafe)

            for unsafe in unsafe_documents:
                contract = copy.deepcopy(self.contracts[path])
                contract["topology_sha256"] = topology_sha256(
                    unsafe,
                    gf_revision=self.policy["gloriousflywheel_revision"],
                )
                with self.assertRaisesRegex(
                    ContractError,
                    "protected trigger contract drifted",
                ):
                    validate_protected_workflow(
                        unsafe,
                        parsed_source,
                        contract,
                        self.policy,
                    )

    def test_escaped_uses_and_runs_on_keys_are_rejected(self) -> None:
        path = ".github/workflows/ci.yml"
        source = self.sources[path]
        variants = [
            source.replace(
                "        uses:",
                '        "u\\u0073es":',
                1,
            ),
            source.replace(
                "    runs-on:",
                '    "r\\u0075ns-on":',
                1,
            ),
        ]
        for unsafe in variants:
            self.assert_protected_rejected(path, unsafe)

    def test_alias_duplicate_and_extra_topology_is_rejected(self) -> None:
        path = ".github/workflows/ci.yml"
        source = self.sources[path]
        variants = [
            source.replace(
                "    runs-on: tinyland-nix",
                "    runs-on: &authority-lane tinyland-nix",
                1,
            ),
            source.replace(
                "  linux-source:\n",
                "  linux-source: &linux-job\n",
                1,
            ).replace(
                "  windows-cross:\n",
                "  windows-cross:\n    <<: *linux-job\n",
                1,
            ),
            source.replace(
                "jobs:\n  linux-source:",
                "jobs:\n  linux-source: {}\n  linux-source:",
                1,
            ),
            source
            + (
                "\n  bypass:\n"
                "    runs-on: tinyland-nix\n"
                "    steps:\n"
                "      - name: Bypass\n"
                "        run: 'true'\n"
            ),
            source.replace(
                "      - name: Verify exact checked out revision",
                "      - name: Extra step\n"
                "        run: 'true'\n\n"
                "      - name: Verify exact checked out revision",
                1,
            ),
        ]
        for unsafe in variants:
            self.assert_protected_rejected(path, unsafe)

    def test_dynamic_list_and_reusable_runners_are_rejected(self) -> None:
        path = ".github/workflows/ci.yml"
        source = self.sources[path]
        variants = [
            source.replace(
                "runs-on: tinyland-nix",
                "runs-on: ${{ vars.RUNNER || 'tinyland-nix' }}",
                1,
            ),
            source.replace(
                "runs-on: tinyland-nix",
                "runs-on: [self-hosted, tinyland-nix]",
                1,
            ),
            source.replace(
                "    runs-on: tinyland-nix",
                "    uses: example/repository/.github/workflows/ci.yml@main\n"
                "    runs-on: tinyland-nix",
                1,
            ),
            source.replace("runs-on: tinyland-nix", "runs-on: ubuntu-latest", 1),
        ]
        for unsafe in variants:
            self.assert_protected_rejected(path, unsafe)

    def test_protected_jobs_require_exact_same_repository_fork_guard(self) -> None:
        for path in sorted(self.contracts):
            document, source = load_workflow(self.root / path)
            for job_name in sorted(document["jobs"]):
                variants: list[dict[str, Any]] = []

                missing = copy.deepcopy(document)
                del missing["jobs"][job_name]["if"]
                variants.append(missing)

                negated = copy.deepcopy(document)
                negated["jobs"][job_name]["if"] = (
                    "${{ github.event_name != 'pull_request' || "
                    "github.event.pull_request.head.repo.full_name != "
                    "github.repository }}"
                )
                variants.append(negated)

                unconditional = copy.deepcopy(document)
                unconditional["jobs"][job_name]["if"] = "${{ always() }}"
                variants.append(unconditional)

                partial = copy.deepcopy(document)
                partial["jobs"][job_name]["if"] = (
                    "${{ github.event_name != 'pull_request' }}"
                )
                variants.append(partial)

                for unsafe in variants:
                    contract = copy.deepcopy(self.contracts[path])
                    contract["topology_sha256"] = topology_sha256(
                        unsafe,
                        gf_revision=self.policy["gloriousflywheel_revision"],
                    )
                    with self.assertRaisesRegex(
                        ContractError,
                        "exact same-repository fork guard",
                    ):
                        validate_protected_workflow(
                            unsafe,
                            source,
                            contract,
                            self.policy,
                        )

    def test_semantic_topology_binds_bodies_order_and_non_gf_actions(self) -> None:
        path = ".github/workflows/ci.yml"
        document, _ = load_workflow(self.root / path)
        contract = self.contracts[path]
        gf_revision = self.policy["gloriousflywheel_revision"]
        linux_steps = document["jobs"]["linux-source"]["steps"]
        action = next(
            step
            for step in linux_steps
            if str(step.get("uses", "")).startswith(GF_ACTION_ROOT)
        )

        body_drift = copy.deepcopy(document)
        body_action = next(
            step
            for step in body_drift["jobs"]["linux-source"]["steps"]
            if str(step.get("uses", "")).startswith(GF_ACTION_ROOT)
        )
        body_action["with"]["command"] += "\n# one-byte-authority-drift"

        run_drift = copy.deepcopy(document)
        run_drift["jobs"]["linux-source"]["steps"][1]["run"] += " "

        order_drift = copy.deepcopy(document)
        order_steps = order_drift["jobs"]["linux-source"]["steps"]
        order_steps[0], order_steps[1] = order_steps[1], order_steps[0]

        checkout_drift = copy.deepcopy(document)
        checkout_drift["jobs"]["linux-source"]["steps"][0]["uses"] = (
            "actions/checkout@" + "0" * 40
        )

        self.assertIn("command", action["with"])
        for unsafe in (body_drift, run_drift, order_drift, checkout_drift):
            with self.assertRaisesRegex(ContractError, "topology drifted"):
                validate_topology(
                    unsafe,
                    contract,
                    gf_revision=gf_revision,
                )

    def test_gf_revision_normalization_is_policy_bound(self) -> None:
        path = ".github/workflows/ci.yml"
        document, source = load_workflow(self.root / path)
        current_revision = self.policy["gloriousflywheel_revision"]
        next_revision = "1" * 40
        changed = copy.deepcopy(document)
        for job in changed["jobs"].values():
            for step in job["steps"]:
                if step.get("uses") == (f"{GF_ACTION_ROOT}nix-job@{current_revision}"):
                    step["uses"] = f"{GF_ACTION_ROOT}nix-job@{next_revision}"

        self.assertEqual(
            topology_sha256(document, gf_revision=current_revision),
            topology_sha256(changed, gf_revision=next_revision),
        )
        self.assertNotEqual(
            topology_sha256(document, gf_revision=current_revision),
            topology_sha256(changed, gf_revision=current_revision),
        )

        contract = copy.deepcopy(self.contracts[path])
        contract["topology_sha256"] = topology_sha256(
            changed,
            gf_revision=current_revision,
        )
        with self.assertRaises(ContractError):
            validate_protected_workflow(
                changed,
                source,
                contract,
                self.policy,
            )

    def test_accept_flake_config_variants_are_rejected(self) -> None:
        path = ".github/workflows/ci.yml"
        source = self.sources[path]
        for token in (
            "--accept-flake-config",
            "--option accept-flake-config true",
            "NIX_CONFIG='accept-flake-config = true'",
        ):
            with self.assertRaisesRegex(
                ContractError,
                "must not accept checked-in flake config",
            ):
                document, _ = load_workflow(self.root / path)
                validate_protected_workflow(
                    document,
                    source + f"\n# {token}\n",
                    self.contracts[path],
                    self.policy,
                )

    def test_workload_cannot_escape_gf_wrapper(self) -> None:
        path = ".github/workflows/ci.yml"
        document, source = load_workflow(self.root / path)
        gf_revision = self.policy["gloriousflywheel_revision"]

        escaped = copy.deepcopy(document)
        escaped["jobs"]["linux-source"]["steps"].append(
            {
                "name": "Escaped workload",
                "run": "nix develop .#default --command cargo test",
            }
        )
        escaped_contract = copy.deepcopy(self.contracts[path])
        escaped_contract["topology_sha256"] = topology_sha256(
            escaped,
            gf_revision=gf_revision,
        )
        with self.assertRaisesRegex(ContractError, "work after the GF wrapper"):
            validate_protected_workflow(
                escaped,
                source,
                escaped_contract,
                self.policy,
            )

        missing = copy.deepcopy(document)
        missing_action = next(
            step
            for step in missing["jobs"]["linux-source"]["steps"]
            if str(step.get("uses", "")).startswith(GF_ACTION_ROOT)
        )
        missing_action["with"]["command"] = missing_action["with"]["command"].replace(
            "cargo deny check", "true", 1
        )
        missing_contract = copy.deepcopy(self.contracts[path])
        missing_contract["topology_sha256"] = topology_sha256(
            missing,
            gf_revision=gf_revision,
        )
        missing_contract["command_sha256"]["linux-source"] = hashlib.sha256(
            missing_action["with"]["command"].encode()
        ).hexdigest()
        with self.assertRaisesRegex(ContractError, "workload escaped"):
            validate_protected_workflow(
                missing,
                source,
                missing_contract,
                self.policy,
            )

    def test_public_key_and_flake_tuple_are_exact(self) -> None:
        path = ".github/workflows/ci.yml"
        document, source = load_workflow(self.root / path)
        changed = copy.deepcopy(document)
        action = next(
            step
            for step in changed["jobs"]["linux-source"]["steps"]
            if str(step.get("uses", "")).startswith(GF_ACTION_ROOT)
        )
        action["with"]["attic-public-key"] = "main:" + "A" * 44
        contract = copy.deepcopy(self.contracts[path])
        contract["topology_sha256"] = topology_sha256(
            changed,
            gf_revision=self.policy["gloriousflywheel_revision"],
        )
        with self.assertRaisesRegex(ContractError, "attic-public-key"):
            validate_protected_workflow(
                changed,
                source,
                contract,
                self.policy,
            )

        changed_policy = copy.deepcopy(self.policy)
        changed_policy["attic_public_read"]["public_key"] = "main:" + "A" * 44
        with self.assertRaisesRegex(ContractError, "policy drifted"):
            validate_attic_public_read(self.root, changed_policy)

        with tempfile.TemporaryDirectory() as temporary:
            candidate = Path(temporary)
            flake = (self.root / "flake.nix").read_text(encoding="utf-8")
            (candidate / "flake.nix").write_text(
                flake.replace(ATTIC_PUBLIC_KEY, "main:" + "A" * 44, 1),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ContractError, "tuple drifted"):
                validate_attic_public_read(candidate, self.policy)

    def test_policy_json_rejects_duplicate_keys(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "config").mkdir()
            (root / "config/ci-authority-policy.json").write_text(
                '{"version": 2, "version": 2}',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ContractError, "duplicate mapping key"):
                load_policy(root)

    def test_cache_and_action_authority_regressions_are_rejected(self) -> None:
        path = ".github/workflows/ci.yml"
        source = self.sources[path]
        checkout = self.policy["checkout_revision"]
        gf_revision = self.policy["gloriousflywheel_revision"]
        variants = [
            source + "\n# actions/cache@" + "0" * 40 + "\n",
            source + "\n# cache-to: type=gha,mode=max\n",
            source.replace(f"actions/checkout@{checkout}", "actions/checkout@main"),
            source.replace(
                f"{GF_ACTION_ROOT}nix-job@{gf_revision}",
                f"{GF_ACTION_ROOT}nix-job@{'0' * 40}",
                1,
            ),
            source.replace("persist-credentials: false", "persist-credentials: true"),
            source.replace(
                '          require-cache-push: "false"',
                '          require-cache-push: "true"',
                1,
            ),
            source.replace(
                '          ATTIC_TOKEN: ""',
                "          ATTIC_TOKEN: inherited",
                1,
            ),
        ]
        for unsafe in variants:
            self.assert_protected_rejected(path, unsafe)

    def test_held_platform_gate_cannot_turn_green(self) -> None:
        path = ".github/workflows/ci.yml"
        unsafe = self.sources[path].replace(
            "          exit 1\n",
            "          exit 0\n",
            1,
        )
        self.assert_protected_rejected(path, unsafe)

    def test_live_storage_image_tags_are_rejected(self) -> None:
        path = ".github/workflows/ci-live-storage.yml"
        unsafe = self.sources[path].replace(
            SEAWEED_IMAGE,
            "chrislusf/seaweedfs:latest",
        )
        self.assert_protected_rejected(path, unsafe)

    def test_new_workflow_requires_ledger_classification(self) -> None:
        policy = copy.deepcopy(self.policy)
        policy["disabled_legacy_workflows"] = policy["disabled_legacy_workflows"][:-1]
        with self.assertRaises(ContractError):
            validate_inventory(self.root, policy)

    def test_disabled_job_parity_is_exact_and_fail_closed(self) -> None:
        entry = self.policy["disabled_legacy_workflows"][0]
        document, _ = load_workflow(self.root / entry["path"])
        job_name = next(iter(document["jobs"]))
        variants: list[dict[str, Any]] = []

        missing = copy.deepcopy(entry)
        del missing["job_parity"][job_name]
        variants.append(missing)

        unowned = copy.deepcopy(entry)
        unowned["job_parity"][job_name]["owner"] = "TIN-1"
        variants.append(unowned)

        unblocked = copy.deepcopy(entry)
        unblocked["job_parity"][job_name]["blocked_by"] = None
        variants.append(unblocked)

        held_as_replacement = copy.deepcopy(entry)
        held_as_replacement["job_parity"][job_name] = {
            "disposition": "migrated",
            "owner": "TIN-2538",
            "replacement": {
                "path": ".github/workflows/ci.yml",
                "job": "darwin-authority-held",
                "runner_class": "tinyland-nix",
            },
            "blocked_by": None,
            "reason": "Invalid migration to a held job.",
        }
        variants.append(held_as_replacement)

        for unsafe in variants:
            with self.assertRaises(ContractError):
                validate_disabled_workflow(
                    document,
                    unsafe,
                    gf_revision=self.policy["gloriousflywheel_revision"],
                    protected_job_index=protected_job_index(self.policy),
                )

    def test_disabled_workflow_cannot_reactivate_or_grow(self) -> None:
        entry = self.policy["disabled_legacy_workflows"][0]
        path = self.root / entry["path"]
        source = path.read_text(encoding="utf-8")
        variants = [
            source.replace(
                f"if: {DISABLED_JOB_CONDITION}",
                "if: ${{ github.repository == github.repository }}",
                1,
            ),
            source
            + (
                "\n  extra-disabled:\n"
                f"    if: {DISABLED_JOB_CONDITION}\n"
                "    runs-on: ubuntu-latest\n"
                "    steps:\n"
                "      - run: 'true'\n"
            ),
        ]
        for unsafe in variants:
            with tempfile.TemporaryDirectory() as temporary:
                candidate = Path(temporary) / "workflow.yml"
                candidate.write_text(unsafe, encoding="utf-8")
                with self.assertRaises(ContractError):
                    document, _ = load_workflow(candidate)
                    validate_disabled_workflow(
                        document,
                        entry,
                        gf_revision=self.policy["gloriousflywheel_revision"],
                        protected_job_index=protected_job_index(self.policy),
                    )


def print_topology(root: Path, policy: dict[str, Any]) -> None:
    entries = policy["protected_proof"] + policy["disabled_legacy_workflows"]
    report = {}
    for entry in entries:
        document, _ = load_workflow(root / entry["path"])
        report[entry["path"]] = topology_sha256(
            document,
            gf_revision=policy["gloriousflywheel_revision"],
        )
    print(json.dumps(report, indent=2, sort_keys=True))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--source-hold-ok",
        action="store_true",
        help="validate the source shape while retaining the explicit TIN-3127 hold",
    )
    parser.add_argument(
        "--print-topology",
        action="store_true",
        help="print reviewed workflow topology digests",
    )
    args = parser.parse_args()
    root = find_repo_root()
    policy = load_policy(root)
    if args.print_topology:
        print_topology(root, policy)
        return 0

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(CiAuthorityContractTest)
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    if not result.wasSuccessful():
        return 1
    try:
        validate_promotable_revision(policy)
    except PromotionHold as hold:
        print(f"HOLD: {hold}", file=sys.stderr)
        return 0 if args.source_hold_ok else 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
