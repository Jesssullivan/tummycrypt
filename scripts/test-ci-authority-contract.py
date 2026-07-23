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
PUBLIC_KEY_EXPRESSION = "${{ vars.ATTIC_PUBLIC_KEY || '' }}"
EXPECTED_SHA_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && "
    "github.event.pull_request.head.sha || github.sha }}"
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
        (root / "config/ci-authority-policy.json").read_text(encoding="utf-8")
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


def scrub_topology(value: Any, *, key: str | None = None) -> Any:
    if key in {"run", "command"}:
        return "<script>"
    if key == "uses" and isinstance(value, str) and value.startswith(GF_ACTION_ROOT):
        return f"{value.rsplit('@', 1)[0]}@<GF_REVISION>"
    if isinstance(value, dict):
        return {
            item_key: scrub_topology(item_value, key=item_key)
            for item_key, item_value in value.items()
        }
    if isinstance(value, list):
        return [scrub_topology(item) for item in value]
    return value


def topology_sha256(document: dict[str, Any]) -> str:
    topology = scrub_topology(document)
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
) -> None:
    expected = contract.get("topology_sha256")
    if not isinstance(expected, str) or re.fullmatch(r"[0-9a-f]{64}", expected) is None:
        raise ContractError(f"{contract['path']} lacks a reviewed topology digest")
    actual = topology_sha256(document)
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
    if action_env.get("ATTIC_TOKEN") != "":
        raise ContractError(f"{job_name} must explicitly clear ATTIC_TOKEN")
    if action_env.get("GF_EXPECTED_RUNNER_ENVIRONMENT") != (
        "${{ runner.environment }}"
    ):
        raise ContractError(f"{job_name} must bind runner.environment explicitly")

    exact_public_values = {
        "attic-enabled": "true",
        "attic-public-key": PUBLIC_KEY_EXPRESSION,
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
    command = action_with.get("command")
    if not isinstance(command, str) or not command:
        raise ContractError(f"{job_name} GF command is missing")
    for required in (
        'test "${GF_EXPECTED_RUNNER_ENVIRONMENT:-}" = self-hosted',
        'test "${ATTIC_TOKEN:-}" = ""',
        'test -n "${ATTIC_SERVER:-}"',
        'test "${ATTIC_CACHE:-}" = main',
        'test -n "${ATTIC_PUBLIC_KEY:-}"',
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


def validate_held_job(job_name: str, job: dict[str, Any], issue: str) -> None:
    if "if" in job or "uses" in job:
        raise ContractError(f"{job_name} must be an unconditional held job")
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
    validate_topology(document, contract)
    validate_permissions(document)
    path = contract["path"]
    expected_jobs = contract["jobs"]
    held_jobs = contract["held_jobs"]
    front_door = contract["front_door"]
    if not isinstance(path, str) or not isinstance(expected_jobs, dict):
        raise ContractError("protected policy entry has an invalid shape")
    if not isinstance(held_jobs, dict) or front_door != "nix-job":
        raise ContractError("protected workflows must use the nix-job front door")

    if HOSTED_RUNNER_PATTERN.search(source):
        raise ContractError("GitHub-hosted runner label is forbidden")
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
        if job_name in held_jobs:
            validate_held_job(job_name, job, held_jobs[job_name])
        else:
            if "if" in job or "uses" in job:
                raise ContractError(f"{job_name} must execute without a bypass")
            validate_public_read_tuple(
                path,
                job_name,
                job,
                checkout_revision=policy["checkout_revision"],
                gf_revision=policy["gloriousflywheel_revision"],
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
) -> None:
    validate_topology(document, contract)
    expected_triggers = contract["triggers"]
    actual_triggers = workflow_triggers(document)
    if actual_triggers != expected_triggers:
        raise ContractError(
            "disabled workflow trigger inventory drifted "
            f"(actual={actual_triggers}, expected={expected_triggers})"
        )
    jobs = workflow_jobs(document)
    for job_name, job in jobs.items():
        if job.get("if") != DISABLED_JOB_CONDITION:
            raise ContractError(
                f"disabled workflow job can allocate a runner: {job_name}"
            )


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
        validate_disabled_workflow(document, entry)


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


def validate_current_tree(root: Path, policy: dict[str, Any]) -> None:
    live_hold = policy.get("live_proof_hold")
    if (
        not isinstance(live_hold, dict)
        or live_hold.get("issue") != "TIN-3120"
        or "GitHub-hosted capacity is never a fallback"
        not in str(live_hold.get("reason", ""))
    ):
        raise ContractError("TIN-3120 live-proof hold or no-fallback claim drifted")
    validate_inventory(root, policy)
    validate_actionlint_labels(root, policy)
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
                    validate_disabled_workflow(document, entry)


def print_topology(root: Path, policy: dict[str, Any]) -> None:
    entries = policy["protected_proof"] + policy["disabled_legacy_workflows"]
    report = {}
    for entry in entries:
        document, _ = load_workflow(root / entry["path"])
        report[entry["path"]] = topology_sha256(document)
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
