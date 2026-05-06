#!/usr/bin/env python3
"""
Plan or apply App Store Connect provisioning for the TCFS macOS FileProvider
testing-mode lab lane.

The script intentionally keeps remote Apple mutations behind --apply. Its
default mode only resolves the desired bundle IDs, device, and certificate, then
prints the profile operations it would perform.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import plistlib
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


API_ROOT = "https://api.appstoreconnect.apple.com/v1"
DEFAULT_CONFIG = "config/macos-fileprovider-lab.asc.json"


class ASCError(RuntimeError):
    pass


def log(message: str) -> None:
    print(message, file=sys.stderr)


def fail(message: str) -> None:
    raise ASCError(message)


def expand_path(value: str) -> Path:
    return Path(os.path.expandvars(os.path.expanduser(value)))


def b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def der_read_length(der: bytes, offset: int) -> tuple[int, int]:
    first = der[offset]
    offset += 1
    if first < 0x80:
        return first, offset
    count = first & 0x7F
    length = int.from_bytes(der[offset : offset + count], "big")
    return length, offset + count


def der_ecdsa_to_raw_rs(der: bytes) -> bytes:
    if not der or der[0] != 0x30:
        fail("openssl returned a non-DER ECDSA signature")
    _, offset = der_read_length(der, 1)
    if der[offset] != 0x02:
        fail("ECDSA signature is missing r integer")
    r_len, offset = der_read_length(der, offset + 1)
    r_bytes = der[offset : offset + r_len]
    offset += r_len
    if der[offset] != 0x02:
        fail("ECDSA signature is missing s integer")
    s_len, offset = der_read_length(der, offset + 1)
    s_bytes = der[offset : offset + s_len]
    r = int.from_bytes(r_bytes, "big")
    s = int.from_bytes(s_bytes, "big")
    return r.to_bytes(32, "big") + s.to_bytes(32, "big")


def openssl_sign_es256(private_key_pem: str, message: str) -> bytes:
    with tempfile.TemporaryDirectory(prefix="tcfs-asc-jwt.") as tmp:
        key_path = Path(tmp) / "AuthKey.p8"
        msg_path = Path(tmp) / "message"
        key_path.write_text(private_key_pem, encoding="utf-8")
        key_path.chmod(0o600)
        msg_path.write_text(message, encoding="utf-8")
        result = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(key_path), str(msg_path)],
            check=False,
            capture_output=True,
        )
    if result.returncode != 0:
        fail(f"openssl failed to sign ASC JWT: {result.stderr.decode().strip()}")
    return der_ecdsa_to_raw_rs(result.stdout)


def make_jwt(key_id: str, issuer_id: str, private_key_pem: str) -> str:
    now = int(time.time())
    header = {"alg": "ES256", "kid": key_id, "typ": "JWT"}
    payload = {
        "iss": issuer_id,
        "iat": now,
        "exp": now + 1200,
        "aud": "appstoreconnect-v1",
    }
    header_b64 = b64url(json.dumps(header, separators=(",", ":")).encode())
    payload_b64 = b64url(json.dumps(payload, separators=(",", ":")).encode())
    message = f"{header_b64}.{payload_b64}"
    signature = openssl_sign_es256(private_key_pem, message)
    return f"{message}.{b64url(signature)}"


def load_config(path: Path) -> dict[str, Any]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        fail(f"config not found: {path}")
    except json.JSONDecodeError as exc:
        fail(f"invalid JSON config {path}: {exc}")


def env_first(*names: str) -> str:
    for name in names:
        value = os.environ.get(name)
        if value:
            return value
    return ""


def private_key_from_env_or_config(asc_config: dict[str, Any]) -> str:
    raw = env_first(
        "ASC_PRIVATE_KEY_P8",
        "APP_STORE_CONNECT_PRIVATE_KEY",
        "APPSTORE_CONNECT_PRIVATE_KEY",
    )
    if raw:
        return raw.replace("\\n", "\n")

    raw_b64 = env_first(
        "ASC_PRIVATE_KEY_BASE64",
        "APP_STORE_CONNECT_PRIVATE_KEY_BASE64",
        "APPSTORE_CONNECT_PRIVATE_KEY_BASE64",
    )
    if raw_b64:
        return base64.b64decode(raw_b64).decode("utf-8")

    path_value = env_first(
        "ASC_PRIVATE_KEY_PATH",
        "APP_STORE_CONNECT_PRIVATE_KEY_PATH",
        "APPSTORE_CONNECT_PRIVATE_KEY_PATH",
    ) or str(asc_config.get("private_key_path", ""))
    if not path_value:
        fail("set ASC_PRIVATE_KEY_PATH or private_key_path in config")

    path = expand_path(path_value)
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        fail(f"ASC private key not found: {path}")


def local_provisioning_udid() -> str:
    system_profiler = shutil.which("system_profiler") or "/usr/sbin/system_profiler"
    if not Path(system_profiler).exists():
        fail("failed to find system_profiler for local Provisioning UDID lookup")
    result = subprocess.run(
        [system_profiler, "SPHardwareDataType"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail("failed to query local Provisioning UDID with system_profiler")
    for line in result.stdout.splitlines():
        if "Provisioning UDID:" in line:
            return line.split(":", 1)[1].strip()
    fail("system_profiler did not return a Provisioning UDID")


def normalize_sha1(value: str) -> str:
    return "".join(ch for ch in value.upper() if ch in "0123456789ABCDEF")


def sha1_from_der_certificate(der: bytes) -> str:
    return hashlib.sha1(der).hexdigest().upper()


def profile_plist_from_bytes(profile_bytes: bytes) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="tcfs-profile-decode.") as tmp:
        profile = Path(tmp) / "profile.provisionprofile"
        out = Path(tmp) / "profile.plist"
        profile.write_bytes(profile_bytes)
        security = subprocess.run(
            ["security", "cms", "-D", "-i", str(profile)],
            check=False,
            capture_output=True,
        )
        if security.returncode == 0:
            out.write_bytes(security.stdout)
        else:
            openssl = subprocess.run(
                [
                    "openssl",
                    "cms",
                    "-verify",
                    "-inform",
                    "DER",
                    "-noverify",
                    "-in",
                    str(profile),
                    "-out",
                    str(out),
                ],
                check=False,
                capture_output=True,
            )
            if openssl.returncode != 0:
                fail("could not decode downloaded provisioning profile")
        return plistlib.loads(out.read_bytes())


def profile_has_required_entitlements(
    profile_bytes: bytes, required: dict[str, Any]
) -> bool:
    if not required:
        return True
    plist = profile_plist_from_bytes(profile_bytes)
    entitlements = plist.get("Entitlements", {})
    for key, expected in required.items():
        if entitlements.get(key) != expected:
            return False
    return True


@dataclass
class ASCClient:
    token: str
    api_root: str = API_ROOT

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        url = f"{self.api_root}{path}"
        if query:
            url = f"{url}?{urllib.parse.urlencode(query)}"
        data = None
        headers = {
            "Authorization": f"Bearer {self.token}",
            "Accept": "application/json",
        }
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                payload = response.read()
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")
            fail(f"ASC {method} {path} failed with HTTP {exc.code}: {detail}")
        if not payload:
            return {}
        return json.loads(payload.decode("utf-8"))

    def get_all(
        self, path: str, query: dict[str, str] | None = None
    ) -> list[dict[str, Any]]:
        next_url = None
        items: list[dict[str, Any]] = []
        while True:
            if next_url:
                request = urllib.request.Request(
                    next_url,
                    headers={
                        "Authorization": f"Bearer {self.token}",
                        "Accept": "application/json",
                    },
                    method="GET",
                )
                with urllib.request.urlopen(request, timeout=60) as response:
                    payload = json.loads(response.read().decode("utf-8"))
            else:
                payload = self.request("GET", path, query=query)
            items.extend(payload.get("data", []))
            next_url = payload.get("links", {}).get("next")
            if not next_url:
                return items


def relationship_ids(resource: dict[str, Any], key: str) -> list[str]:
    data = resource.get("relationships", {}).get(key, {}).get("data")
    if data is None:
        return []
    if isinstance(data, list):
        return [entry["id"] for entry in data]
    return [data["id"]]


def resolve_bundle_id(
    client: ASCClient,
    bundle_identifier: str,
    team_id: str,
    required_capabilities: set[str] | None = None,
) -> dict[str, Any]:
    rows = client.get_all(
        "/bundleIds",
        {
            "filter[identifier]": bundle_identifier,
            "filter[platform]": "MAC_OS",
            "fields[bundleIds]": "identifier,name,platform,seedId",
            "limit": "200",
        },
    )
    if not rows:
        fail(f"ASC bundle ID not found for {bundle_identifier}")
    if team_id:
        team_rows = [
            row for row in rows if row.get("attributes", {}).get("seedId") == team_id
        ]
        if team_rows:
            rows = team_rows
    if required_capabilities:
        capable_rows = []
        for row in rows:
            capabilities = maybe_list_capabilities(client, row["id"])
            if required_capabilities <= capabilities:
                capable_rows.append(row)
        if capable_rows:
            rows = capable_rows
    if len(rows) > 1:
        ids = ", ".join(
            f"{row.get('id')} seed={row.get('attributes', {}).get('seedId', 'unknown')}"
            for row in rows
        )
        fail(f"ASC returned multiple bundle IDs for {bundle_identifier}: {ids}")
    return rows[0]


def resolve_device(client: ASCClient, udid: str) -> dict[str, Any]:
    rows = client.get_all(
        "/devices",
        {
            "filter[udid]": udid,
            "fields[devices]": "name,platform,udid,deviceClass,status,model",
            "limit": "200",
        },
    )
    active = [row for row in rows if row.get("attributes", {}).get("status") == "ENABLED"]
    candidates = active or rows
    if not candidates:
        fail(f"ASC device not found for UDID {udid}")
    if len(candidates) > 1:
        fail(f"ASC returned multiple devices for UDID {udid}")
    return candidates[0]


def list_certificate_candidates(
    client: ASCClient, certificate_types: list[str]
) -> list[dict[str, Any]]:
    candidates: list[dict[str, Any]] = []
    fields = "name,certificateType,displayName,serialNumber,platform,expirationDate,certificateContent"
    for cert_type in certificate_types:
        candidates.extend(
            client.get_all(
                "/certificates",
                {
                    "filter[certificateType]": cert_type,
                    "fields[certificates]": fields,
                    "limit": "200",
                },
            )
        )
    return candidates


def certificate_content_der(cert: dict[str, Any]) -> bytes:
    content = cert.get("attributes", {}).get("certificateContent")
    if not content:
        fail(f"certificate {cert.get('id')} did not include certificateContent")
    return base64.b64decode(content)


def resolve_certificate_by_sha1(
    client: ASCClient, sha1: str, certificate_types: list[str]
) -> tuple[dict[str, Any], bytes]:
    wanted = normalize_sha1(sha1)
    if not wanted:
        fail("set --certificate-sha1 or signing.certificate_sha1")
    for cert in list_certificate_candidates(client, certificate_types):
        der = certificate_content_der(cert)
        if sha1_from_der_certificate(der) == wanted:
            return cert, der
    fail(f"ASC certificate not found for SHA-1 {wanted}")


def revoke_certificate_by_sha1(
    client: ASCClient, sha1: str, certificate_types: list[str]
) -> str:
    wanted = normalize_sha1(sha1)
    if len(wanted) != 40:
        fail("--revoke-certificate-sha1 must be a full 40-hex SHA-1 fingerprint")
    for cert in list_certificate_candidates(client, certificate_types):
        der = certificate_content_der(cert)
        if sha1_from_der_certificate(der) == wanted:
            client.request("DELETE", f"/certificates/{cert['id']}")
            return str(cert["id"])
    fail(f"ASC certificate not found for SHA-1 {wanted}")


def create_certificate_from_csr(
    client: ASCClient, certificate_type: str, csr_content: str
) -> tuple[dict[str, Any], bytes]:
    payload = {
        "data": {
            "type": "certificates",
            "attributes": {
                "certificateType": certificate_type,
                "csrContent": csr_content,
            },
        }
    }
    cert = client.request("POST", "/certificates", body=payload)["data"]
    der = certificate_content_der(cert)
    return cert, der


def active_profiles_by_name(client: ASCClient, name: str) -> list[dict[str, Any]]:
    return client.get_all(
        "/profiles",
        {
            "filter[name]": name,
            "filter[profileState]": "ACTIVE",
            "fields[profiles]": "name,profileType,profileState,uuid,createdDate,expirationDate",
            "include": "bundleId,certificates,devices",
            "limit": "200",
        },
    )


def delete_profile(client: ASCClient, profile_id: str) -> None:
    client.request("DELETE", f"/profiles/{profile_id}")


def create_profile(
    client: ASCClient,
    name: str,
    profile_type: str,
    bundle_id: str,
    certificate_id: str,
    device_id: str,
) -> dict[str, Any]:
    payload = {
        "data": {
            "type": "profiles",
            "attributes": {
                "name": name,
                "profileType": profile_type,
            },
            "relationships": {
                "bundleId": {"data": {"type": "bundleIds", "id": bundle_id}},
                "certificates": {
                    "data": [{"type": "certificates", "id": certificate_id}]
                },
                "devices": {"data": [{"type": "devices", "id": device_id}]},
            },
        }
    }
    return client.request("POST", "/profiles", body=payload)["data"]


def read_profile(client: ASCClient, profile_id: str) -> dict[str, Any]:
    return client.request(
        "GET",
        f"/profiles/{profile_id}",
        query={
            "fields[profiles]": "name,profileType,profileState,uuid,createdDate,expirationDate,profileContent",
            "include": "bundleId,certificates,devices",
        },
    )["data"]


def downloaded_profile_bytes(client: ASCClient, profile_id: str) -> bytes:
    profile = read_profile(client, profile_id)
    content = profile.get("attributes", {}).get("profileContent")
    if not content:
        fail(f"profile {profile_id} did not include profileContent")
    return base64.b64decode(content)


def profile_matches_desired(
    profile: dict[str, Any],
    bundle_id: str,
    certificate_id: str,
    device_id: str,
) -> bool:
    return (
        relationship_ids(profile, "bundleId") == [bundle_id]
        and certificate_id in relationship_ids(profile, "certificates")
        and device_id in relationship_ids(profile, "devices")
    )


def write_profile(
    profile_bytes: bytes,
    output_dir: Path,
    role: str,
    install_filename: str,
    profiles_dir: Path | None,
) -> tuple[Path, Path | None]:
    output_dir.mkdir(parents=True, exist_ok=True)
    output_path = output_dir / install_filename
    output_path.write_bytes(profile_bytes)

    installed_path = None
    if profiles_dir is not None:
        profiles_dir.mkdir(parents=True, exist_ok=True)
        installed_path = profiles_dir / install_filename
        installed_path.write_bytes(profile_bytes)
        log(f"installed {role} profile: {installed_path}")

    return output_path, installed_path


def generate_key_and_csr(output_dir: Path, common_name: str) -> tuple[Path, str]:
    output_dir.mkdir(parents=True, exist_ok=True)
    key_path = output_dir / "tcfs-fileprovider-lab.key.pem"
    csr_path = output_dir / "tcfs-fileprovider-lab.csr"
    if not key_path.exists():
        result = subprocess.run(
            ["openssl", "genrsa", "-out", str(key_path), "2048"],
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            fail(f"openssl genrsa failed: {result.stderr.strip()}")
        key_path.chmod(0o600)
    result = subprocess.run(
        [
            "openssl",
            "req",
            "-new",
            "-key",
            str(key_path),
            "-out",
            str(csr_path),
            "-subj",
            f"/CN={common_name}",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail(f"openssl req failed: {result.stderr.strip()}")
    return key_path, csr_path.read_text(encoding="utf-8")


def write_certificate_and_p12(
    output_dir: Path,
    cert_der: bytes,
    key_path: Path,
    cert_sha1: str,
    password: str,
) -> tuple[Path, Path]:
    cert_der_path = output_dir / f"tcfs-fileprovider-lab-{cert_sha1[:8]}.cer"
    cert_pem_path = output_dir / f"tcfs-fileprovider-lab-{cert_sha1[:8]}.cert.pem"
    p12_path = output_dir / f"tcfs-fileprovider-lab-{cert_sha1[:8]}.p12"
    cert_der_path.write_bytes(cert_der)

    result = subprocess.run(
        [
            "openssl",
            "x509",
            "-inform",
            "DER",
            "-in",
            str(cert_der_path),
            "-out",
            str(cert_pem_path),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail(f"openssl x509 conversion failed: {result.stderr.strip()}")

    if sys.platform == "darwin" and shutil.which("security"):
        ok, detail = write_security_exported_p12(
            key_path,
            cert_der_path,
            p12_path,
            password,
        )
        if ok:
            p12_path.chmod(0o600)
            return cert_der_path, p12_path
        log(f"warning: macOS security p12 export failed; falling back to openssl: {detail}")
        p12_path.unlink(missing_ok=True)

    result = subprocess.run(
        [
            "openssl",
            "pkcs12",
            "-export",
            "-inkey",
            str(key_path),
            "-in",
            str(cert_pem_path),
            "-out",
            str(p12_path),
            "-passout",
            f"pass:{password}",
            "-name",
            f"TCFS FileProvider Lab {cert_sha1[:8]}",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail(f"openssl pkcs12 export failed: {result.stderr.strip()}")
    p12_path.chmod(0o600)
    return cert_der_path, p12_path


def write_security_exported_p12(
    key_path: Path,
    cert_der_path: Path,
    p12_path: Path,
    p12_password: str,
) -> tuple[bool, str]:
    keychain_password = secrets.token_hex(16)
    with tempfile.TemporaryDirectory(prefix="tcfs-asc-p12-export.") as tmp:
        keychain_path = Path(tmp) / "tcfs-asc-p12-export.keychain-db"

        commands = [
            ["security", "create-keychain", "-p", keychain_password, str(keychain_path)],
            ["security", "unlock-keychain", "-p", keychain_password, str(keychain_path)],
            ["security", "set-keychain-settings", "-lut", "21600", str(keychain_path)],
            [
                "security",
                "import",
                str(key_path),
                "-k",
                str(keychain_path),
                "-A",
                "-T",
                "/usr/bin/codesign",
                "-T",
                "/usr/bin/security",
            ],
            [
                "security",
                "import",
                str(cert_der_path),
                "-k",
                str(keychain_path),
                "-A",
                "-T",
                "/usr/bin/codesign",
                "-T",
                "/usr/bin/security",
            ],
            [
                "security",
                "set-key-partition-list",
                "-S",
                "apple-tool:,apple:,codesign:",
                "-s",
                "-k",
                keychain_password,
                str(keychain_path),
            ],
            [
                "security",
                "export",
                "-k",
                str(keychain_path),
                "-t",
                "identities",
                "-f",
                "pkcs12",
                "-P",
                p12_password,
                "-o",
                str(p12_path),
            ],
        ]

        try:
            for command in commands:
                result = subprocess.run(
                    command,
                    check=False,
                    capture_output=True,
                    text=True,
                )
                if result.returncode != 0:
                    return False, result.stderr.strip() or result.stdout.strip()
        finally:
            subprocess.run(
                ["security", "delete-keychain", str(keychain_path)],
                check=False,
                capture_output=True,
            )

    return True, ""


def maybe_list_capabilities(client: ASCClient, bundle_id: str) -> set[str]:
    try:
        rows = client.get_all(
            f"/bundleIds/{bundle_id}/bundleIdCapabilities",
            {"fields[bundleIdCapabilities]": "capabilityType,settings"},
        )
    except ASCError as exc:
        log(f"warning: could not list bundle capabilities for {bundle_id}: {exc}")
        return set()
    return {row.get("attributes", {}).get("capabilityType", "") for row in rows}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Provision TCFS macOS FileProvider lab profiles through ASC."
    )
    parser.add_argument("--config", default=DEFAULT_CONFIG)
    parser.add_argument("--apply", action="store_true", help="mutate ASC and write profiles")
    parser.add_argument("--replace", action="store_true", help="delete stale active profiles with the desired name")
    parser.add_argument("--create-certificate", action="store_true", help="create a fresh macOS development cert from a local CSR")
    parser.add_argument("--create-certificate-type", default="", help="ASC CertificateType to use with --create-certificate")
    parser.add_argument("--certificate-sha1", default="")
    parser.add_argument("--revoke-certificate-sha1", default="", help="revoke this exact existing cert before creating a replacement")
    parser.add_argument("--device-udid", default="")
    parser.add_argument("--output-dir", default="build/asc-fileprovider-lab")
    parser.add_argument("--profiles-dir", default="~/Library/MobileDevice/Provisioning Profiles")
    parser.add_argument("--install", action="store_true", help="install downloaded profiles into --profiles-dir")
    parser.add_argument("--no-name-suffix", action="store_true", help="do not append the certificate SHA prefix to ASC profile names")
    parser.add_argument("--validate-config", action="store_true", help="validate config and exit without ASC access")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    config_path = Path(args.config)
    config = load_config(config_path)
    team_id = str(config.get("team_id", ""))

    profiles = config.get("profiles", [])
    if not profiles:
        fail("config.profiles must not be empty")
    for profile in profiles:
        for key in ("role", "name", "install_filename", "bundle_id", "profile_type"):
            if not profile.get(key):
                fail(f"profile entry missing {key}")

    if args.validate_config:
        print(f"config valid: {config_path}")
        print(f"profiles: {len(profiles)}")
        return 0

    early_revoke_sha1 = normalize_sha1(args.revoke_certificate_sha1)
    if early_revoke_sha1:
        if not args.apply:
            fail("--revoke-certificate-sha1 requires --apply")
        if not args.create_certificate:
            fail("--revoke-certificate-sha1 requires --create-certificate")

    asc_config = config.get("app_store_connect", {})
    key_id = env_first("ASC_KEY_ID", "APP_STORE_CONNECT_API_KEY_ID") or asc_config.get("key_id", "")
    issuer_id = env_first("ASC_ISSUER_ID", "APP_STORE_CONNECT_ISSUER_ID") or asc_config.get("issuer_id", "")
    if not key_id or not issuer_id:
        fail("ASC key_id and issuer_id are required")
    private_key = private_key_from_env_or_config(asc_config)
    token = make_jwt(key_id, issuer_id, private_key)
    client = ASCClient(token)

    runner = config.get("runner", {})
    device_udid = (
        args.device_udid
        or env_first("TCFS_FILEPROVIDER_LAB_DEVICE_UDID", "PZM_PROVISIONING_UDID")
        or str(runner.get("device_udid", ""))
    )
    if device_udid == "auto":
        device_udid = local_provisioning_udid()
    if not device_udid:
        fail("set --device-udid, TCFS_FILEPROVIDER_LAB_DEVICE_UDID, or runner.device_udid")

    output_dir = Path(args.output_dir)
    profiles_dir = expand_path(args.profiles_dir) if args.install else None

    signing = config.get("signing", {})
    certificate_types = signing.get("certificate_types", ["DEVELOPMENT"])
    cert_der: bytes
    cert: dict[str, Any]
    p12_path: Path | None = None

    revoke_sha1 = normalize_sha1(args.revoke_certificate_sha1)
    if revoke_sha1:
        revoked_id = revoke_certificate_by_sha1(client, revoke_sha1, certificate_types)
        log(f"revoked ASC certificate: {revoked_id} sha1={revoke_sha1}")

    if args.create_certificate:
        if not args.apply:
            fail("--create-certificate requires --apply")
        common_name = f"TCFS FileProvider Lab {runner.get('name', 'mac')}"
        key_path, csr_content = generate_key_and_csr(output_dir, common_name)
        create_certificate_type = (
            args.create_certificate_type
            or signing.get("create_certificate_type", "MAC_APP_DEVELOPMENT")
        )
        cert, cert_der = create_certificate_from_csr(
            client,
            create_certificate_type,
            csr_content,
        )
        cert_sha1 = sha1_from_der_certificate(cert_der)
        password_env = signing.get("p12_password_env", "TCFS_FILEPROVIDER_LAB_P12_PASSWORD")
        p12_password = os.environ.get(password_env, "")
        if not p12_password:
            log(f"warning: {password_env} is empty; generated p12 will use an empty import password")
        _, p12_path = write_certificate_and_p12(
            output_dir, cert_der, key_path, cert_sha1, p12_password
        )
        log(
            f"created ASC certificate: {cert.get('id')} "
            f"type={create_certificate_type} sha1={cert_sha1}"
        )
        log(f"wrote p12: {p12_path}")
    else:
        cert_sha1 = normalize_sha1(
            args.certificate_sha1
            or env_first("TCFS_FILEPROVIDER_LAB_CERT_SHA1", "ASC_CERTIFICATE_SHA1")
            or str(signing.get("certificate_sha1", ""))
        )
        cert, cert_der = resolve_certificate_by_sha1(client, cert_sha1, certificate_types)
        cert_sha1 = sha1_from_der_certificate(cert_der)

    device = resolve_device(client, device_udid)
    print(f"device: {device.get('attributes', {}).get('name')} {device_udid} id={device.get('id')}")
    print(f"certificate: {cert.get('attributes', {}).get('displayName')} sha1={cert_sha1} id={cert.get('id')}")

    written_paths: list[Path] = []
    for desired in profiles:
        required_capabilities = set(desired.get("required_capabilities", []))
        bundle = resolve_bundle_id(
            client, desired["bundle_id"], team_id, required_capabilities
        )
        if required_capabilities:
            actual_capabilities = maybe_list_capabilities(client, bundle["id"])
            missing = sorted(required_capabilities - actual_capabilities)
            if missing:
                fail(
                    f"bundle {desired['bundle_id']} missing capabilities: {', '.join(missing)}"
                )

        suffix = "" if args.no_name_suffix else f"-{cert_sha1[:8].lower()}"
        profile_name = f"{desired['name']}{suffix}"
        print(
            f"profile[{desired['role']}]: {profile_name} "
            f"bundle={desired['bundle_id']} type={desired['profile_type']}"
        )

        existing = active_profiles_by_name(client, profile_name)
        selected = None
        for profile in existing:
            full = read_profile(client, profile["id"])
            if profile_matches_desired(full, bundle["id"], cert["id"], device["id"]):
                selected = full
                print(f"  reusing active profile id={profile['id']}")
                break

        stale = [profile for profile in existing if not selected or profile["id"] != selected["id"]]
        if stale and not args.replace and selected is None:
            stale_ids = ", ".join(profile["id"] for profile in stale)
            fail(f"stale active profile(s) with desired name exist: {stale_ids}; rerun with --replace")

        if args.apply:
            if selected is None:
                if args.replace:
                    for profile in stale:
                        print(f"  deleting stale active profile id={profile['id']}")
                        delete_profile(client, profile["id"])
                selected = create_profile(
                    client,
                    profile_name,
                    desired["profile_type"],
                    bundle["id"],
                    cert["id"],
                    device["id"],
                )
                print(f"  created profile id={selected['id']}")
            profile_bytes = downloaded_profile_bytes(client, selected["id"])
            if not profile_has_required_entitlements(
                profile_bytes, desired.get("required_entitlements", {})
            ):
                fail(f"downloaded {desired['role']} profile is missing required entitlements")
            output_path, installed_path = write_profile(
                profile_bytes,
                output_dir,
                desired["role"],
                desired["install_filename"],
                profiles_dir,
            )
            written_paths.append(installed_path or output_path)
        else:
            print("  plan only; pass --apply to create/download")

    if args.apply:
        print("profile files:")
        for path in written_paths:
            print(f"  {path}")
        if p12_path is not None:
            print(f"signing_p12_path={p12_path}")
        print("next:")
        print(
            "  bash scripts/macos-fileprovider-profile-inventory.sh "
            "--require-host-entitlement com.apple.developer.fileprovider.testing-mode --strict"
        )
        if p12_path is not None:
            print(f"  scripts/macos-codesign-p12-probe.sh --p12 {p12_path}")
            print(
                "  scripts/macos-fileprovider-testing-mode-dispatch.sh "
                "--tag v0.12.11 --runner-label petting-zoo-mini "
                f"--signing-p12-path {p12_path}"
            )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ASCError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
