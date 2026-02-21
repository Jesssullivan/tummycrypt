#!/usr/bin/env bash
# scripts/cross-compile.sh
# Cross-compile tcfsd + tcfs binaries for all target platforms
#
# Prerequisites: cargo-cross installed (cargo install cross)
# Or use Nix: nix build .#tcfsd-aarch64-linux

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${REPO_ROOT}/dist"

echo "==> tcfs cross-compilation"
echo "    Output: ${OUT_DIR}/"

mkdir -p "${OUT_DIR}"

TARGETS=(
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-apple-darwin"
    "aarch64-apple-darwin"
)

BINARIES=("tcfsd" "tcfs" "tcfs-tui")

for target in "${TARGETS[@]}"; do
    echo ""
    echo "==> Building for ${target}..."

    if command -v cross &>/dev/null; then
        cross build --release --target "${target}" --workspace
    else
        cargo build --release --target "${target}" --workspace
    fi

    target_out="${OUT_DIR}/${target}"
    mkdir -p "${target_out}"

    for bin in "${BINARIES[@]}"; do
        src="${REPO_ROOT}/target/${target}/release/${bin}"
        if [[ -f "${src}" ]]; then
            cp "${src}" "${target_out}/${bin}"
            echo "    ${target_out}/${bin}"
        fi
    done
done

echo ""
echo "==> Cross-compilation complete!"
echo "    Output files:"
find "${OUT_DIR}" -type f | sort | sed 's/^/    /'
