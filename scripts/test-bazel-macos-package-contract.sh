#!/usr/bin/env bash
#
# Static regression checks for the TCFS Bazel macOS package contract.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_file() {
  local path="$1"

  [[ -f "$REPO_ROOT/$path" ]] || fail "missing file: $path"
}

require_contains() {
  local path="$1"
  local needle="$2"

  grep -Fq -- "$needle" "$REPO_ROOT/$path" ||
    fail "$path missing expected text: $needle"
}

require_not_contains() {
  local path="$1"
  local needle="$2"

  if grep -Fq -- "$needle" "$REPO_ROOT/$path"; then
    fail "$path contains forbidden text: $needle"
  fi
}

require_file MODULE.bazel
require_file .bazelversion
require_file BUILD.bazel
require_file build/macos/BUILD.bazel
require_file build/macos/darwin_pkg.bzl
require_file scripts/BUILD.bazel
require_file docs/ops/darwin-bazel-package-contract.md
require_file scripts/macos-build-pkg.sh
require_file scripts/macos-pkg-postinstall.sh
require_file scripts/macos-pkg-structure-smoke.sh

require_contains MODULE.bazel 'module('
require_contains MODULE.bazel 'name = "tummycrypt"'
require_contains MODULE.bazel 'version = "0.12.14"'
require_contains MODULE.bazel 'bazel_dep(name = "platforms", version = "1.0.0")'
require_contains MODULE.bazel 'use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_file")'
require_contains MODULE.bazel 'name = "tcfs_v0_12_14_macos_aarch64_cli_tar"'
require_contains MODULE.bazel 'downloaded_file_path = "tcfs-0.12.14-macos-aarch64.tar.gz"'
require_contains MODULE.bazel 'sha256 = "55719e35b624df25386baf63d74247c39a25e857b7c3855cfd4edd6cfae69175"'
require_contains MODULE.bazel 'name = "tcfs_v0_12_14_macos_aarch64_fileprovider_zip"'
require_contains MODULE.bazel 'downloaded_file_path = "TCFSProvider-0.12.14-macos-aarch64.zip"'
require_contains MODULE.bazel 'sha256 = "b5dd29c46073252ba31d4aaa1d07a88a757e33c9a08bef68ea6d3748753c51f4"'
require_contains .bazelversion '9.1.1'

require_contains BUILD.bazel 'darwin_package_fixture_contract'
require_contains BUILD.bazel 'darwin_package_release_artifacts_unsigned'
require_not_contains BUILD.bazel 'name = "darwin_package"'

require_contains build/macos/darwin_pkg.bzl 'ctx.actions.run'
require_contains build/macos/darwin_pkg.bzl '"cli_tar": attr.label('
require_contains build/macos/darwin_pkg.bzl '"fileprovider_zip": attr.label('
require_contains build/macos/darwin_pkg.bzl '"signing_identity": attr.string()'
require_contains build/macos/darwin_pkg.bzl '"TCFS_PKG_STRUCTURE_SMOKE": ctx.executable.structure_smoke.path'
require_contains build/macos/darwin_pkg.bzl '"requires-darwin-packaging-tools": "1"'

require_contains build/macos/BUILD.bazel 'tcfs_macos_pkg('
require_contains build/macos/BUILD.bazel 'name = "darwin_package_fixture_contract"'
require_contains build/macos/BUILD.bazel 'target_compatible_with = ["@platforms//os:macos"]'
require_contains build/macos/BUILD.bazel '"fixture-only"'
require_contains build/macos/BUILD.bazel '"gloriousflywheel-rbe-contract"'
require_contains build/macos/BUILD.bazel 'name = "darwin_package_release_artifacts_unsigned"'
require_contains build/macos/BUILD.bazel '@tcfs_v0_12_14_macos_aarch64_cli_tar//file'
require_contains build/macos/BUILD.bazel '@tcfs_v0_12_14_macos_aarch64_fileprovider_zip//file'
require_contains build/macos/BUILD.bazel '"release-artifact-inputs"'
require_contains build/macos/BUILD.bazel '"unsigned-package"'
require_not_contains build/macos/BUILD.bazel 'name = "darwin_package"'

require_contains scripts/BUILD.bazel '"macos-build-pkg.sh"'
require_contains scripts/BUILD.bazel '"macos-pkg-postinstall.sh"'
require_contains scripts/BUILD.bazel '"macos-pkg-structure-smoke.sh"'

require_contains docs/ops/darwin-bazel-package-contract.md '//build/macos:darwin_package_fixture_contract'
require_contains docs/ops/darwin-bazel-package-contract.md '//build/macos:darwin_package_release_artifacts_unsigned'
require_contains docs/ops/darwin-bazel-package-contract.md "non-fixture"
require_contains docs/ops/darwin-bazel-package-contract.md "unsigned package-assembly target"
require_contains docs/ops/darwin-bazel-package-contract.md "The existing blocked \`//:darwin_package\` placeholder should stay blocked"

help_output="$("$REPO_ROOT/scripts/macos-build-pkg.sh" --help)"
case "$help_output" in
  *"--cli-tar <path>"*"--fileprovider-zip <path>"*"--postinstall <path>"*"--sign <identity>"*) ;;
  *) fail "macos-build-pkg.sh help no longer exposes the Bazel rule inputs" ;;
esac

printf 'Bazel macOS package contract checks passed\n'
