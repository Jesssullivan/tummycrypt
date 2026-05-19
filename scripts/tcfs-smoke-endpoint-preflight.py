#!/usr/bin/env python3
"""Validate TCFS smoke storage endpoint posture for CI runners."""

from __future__ import annotations

import argparse
import ipaddress
import socket
import sys
import urllib.parse


HOSTED_RUNNER_PREFIXES = ("ubuntu-", "macos-", "windows-")
PRIVATE_DNS_SUFFIXES = (".ts.net", ".local", ".internal", ".home.arpa")
TAILNET_NETWORK = ipaddress.ip_network("100.64.0.0/10")


def error(message: str) -> int:
    print(f"::error::{message}")
    return 1


def is_hosted_runner(label: str) -> bool:
    return label.startswith(HOSTED_RUNNER_PREFIXES)


def validate_endpoint_shape(endpoint: str) -> urllib.parse.ParseResult:
    parsed = urllib.parse.urlparse(endpoint)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("TCFS_SMOKE_S3_ENDPOINT must be an http(s) URL with a hostname")
    return parsed


def parse_ip_literal(host: str) -> ipaddress.IPv4Address | ipaddress.IPv6Address | None:
    try:
        return ipaddress.ip_address(host)
    except ValueError:
        return None


def is_non_global(ip: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    return ip in TAILNET_NETWORK or not ip.is_global


def validate_public_host(host: str, platform: str) -> int:
    if host in {"localhost", "localhost.localdomain"}:
        return error(
            f"TCFS_SMOKE_S3_ENDPOINT host {host!r} is local-only and cannot be reached from GitHub-hosted {platform} runners."
        )

    if host.endswith(PRIVATE_DNS_SUFFIXES):
        return error(
            f"TCFS_SMOKE_S3_ENDPOINT host {host!r} looks private to a tailnet or local DNS domain; use a self-hosted {platform} runner."
        )

    literal_ip = parse_ip_literal(host)
    if literal_ip is not None:
        if is_non_global(literal_ip):
            return error(
                f"TCFS_SMOKE_S3_ENDPOINT host {host!r} is not globally routable; use a self-hosted {platform} runner."
            )
        return 0

    if "." not in host:
        return error(
            f"TCFS_SMOKE_S3_ENDPOINT host {host!r} is a bare hostname; GitHub-hosted {platform} runners need a publicly routable DNS name."
        )

    return 0


def resolve_endpoint(host: str, port: int) -> list[socket.AddressInfo]:
    return socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)


def validate_resolved_addresses(
    addrs: list[socket.AddressInfo],
    platform: str,
) -> int:
    non_global = []
    for _family, _socktype, _proto, _canonname, sockaddr in addrs:
        try:
            resolved_ip = ipaddress.ip_address(sockaddr[0])
        except ValueError:
            continue
        if is_non_global(resolved_ip):
            non_global.append(str(resolved_ip))

    if non_global:
        return error(
            "TCFS_SMOKE_S3_ENDPOINT resolves to non-global addresses from this GitHub-hosted "
            f"{platform} runner: {', '.join(sorted(set(non_global)))}. "
            f"Use a self-hosted {platform} runner for private/tailnet smoke backends."
        )

    return 0


def connect_any(addrs: list[socket.AddressInfo], timeout: float) -> tuple[bool, OSError | None]:
    last_error = None
    for family, socktype, proto, _canonname, sockaddr in addrs:
        sock = socket.socket(family, socktype, proto)
        sock.settimeout(timeout)
        try:
            sock.connect(sockaddr)
            return True, None
        except OSError as exc:
            last_error = exc
        finally:
            sock.close()
    return False, last_error


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--runner-label", required=True)
    parser.add_argument("--platform", required=True, choices=("linux", "macOS", "windows"))
    parser.add_argument("--timeout", type=float, default=5.0)
    parser.add_argument(
        "--skip-connect",
        action="store_true",
        help="Only validate endpoint shape/classification; do not resolve or connect.",
    )
    args = parser.parse_args(argv)

    try:
        parsed = validate_endpoint_shape(args.endpoint.strip())
    except ValueError as exc:
        return error(str(exc))

    host = parsed.hostname.rstrip(".").lower()
    hosted_runner = is_hosted_runner(args.runner_label)

    if hosted_runner:
        if parsed.scheme != "https":
            return error(
                "GitHub-hosted "
                f"{args.platform} smoke requires TCFS_SMOKE_S3_ENDPOINT to be an HTTPS URL. "
                "Use a self-hosted runner_label for private/plaintext smoke backends."
            )

        public_host_status = validate_public_host(host, args.platform)
        if public_host_status != 0:
            return public_host_status
    else:
        print(
            f"Self-hosted/private runner label {args.runner_label!r}: allowing private/plaintext storage endpoint classes."
        )

    if args.skip_connect:
        print("storage endpoint posture preflight passed (connectivity skipped)")
        return 0

    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    try:
        addrs = resolve_endpoint(parsed.hostname, port)
    except socket.gaierror as exc:
        print(
            "::error::TCFS_SMOKE_S3_ENDPOINT hostname does not resolve from this "
            f"{args.platform} runner; use a runner_label with private DNS access or a smoke environment with a public endpoint."
        )
        print(f"DNS error: {exc}")
        return 1

    if hosted_runner:
        resolved_status = validate_resolved_addresses(addrs, args.platform)
        if resolved_status != 0:
            return resolved_status

    connected, last_error = connect_any(addrs, args.timeout)
    if connected:
        print("storage endpoint TCP preflight passed")
        return 0

    print(
        "::error::TCFS_SMOKE_S3_ENDPOINT did not accept a TCP connection from this "
        f"{args.platform} runner; use a private runner or a reachable endpoint."
    )
    if last_error is not None:
        print(f"TCP error: {last_error}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
