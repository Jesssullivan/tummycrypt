#!/usr/bin/env bash
# scripts/setup-dev.sh
# Development environment setup for tummycrypt/tcfs
# Nix-first: uses nix develop if available, falls back to manual install

set -euo pipefail

echo "==> tummycrypt/tcfs development environment setup"
echo ""

# Try Nix first
if command -v nix &>/dev/null; then
    echo "==> Nix detected. Using 'nix develop' for reproducible devShell."
    echo "    Run: nix develop"
    echo ""
    echo "    The Nix devShell provides: Rust toolchain, protoc, age, sops,"
    echo "    opentofu, kubectl, helm, task, nats-cli, cargo-deny, cargo-audit"
    exit 0
fi

echo "==> Nix not found. Installing dependencies manually..."
OS="$(uname -s)"

# Rust
if ! command -v rustc &>/dev/null; then
    echo "==> Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    source "${HOME}/.cargo/env"
else
    echo "==> Rust: $(rustc --version)"
fi

# Cargo tools
for tool in cargo-deny cargo-audit cargo-watch; do
    if ! cargo "${tool#cargo-}" --version &>/dev/null 2>&1; then
        echo "==> Installing ${tool}..."
        cargo install "${tool}"
    fi
done

# Task runner
if ! command -v task &>/dev/null; then
    echo "==> Installing Task (go-task)..."
    if [[ "${OS}" == "Linux" ]] && command -v dnf &>/dev/null; then
        sudo dnf install -y go-task 2>/dev/null || \
            sh -c "$(curl --location https://taskfile.dev/install.sh)" -- -d -b /usr/local/bin
    elif [[ "${OS}" == "Linux" ]] && command -v apt-get &>/dev/null; then
        sh -c "$(curl --location https://taskfile.dev/install.sh)" -- -d -b /usr/local/bin
    elif [[ "${OS}" == "Darwin" ]]; then
        brew install go-task/tap/go-task
    fi
fi

# protoc
if ! command -v protoc &>/dev/null; then
    echo "==> Installing protoc..."
    if [[ "${OS}" == "Linux" ]] && command -v dnf &>/dev/null; then
        sudo dnf install -y protobuf-compiler
    elif [[ "${OS}" == "Linux" ]] && command -v apt-get &>/dev/null; then
        sudo apt-get install -y protobuf-compiler
    elif [[ "${OS}" == "Darwin" ]]; then
        brew install protobuf
    fi
fi

# System libraries
if [[ "${OS}" == "Linux" ]]; then
    echo "==> Installing system libraries..."
    if command -v dnf &>/dev/null; then
        sudo dnf install -y rocksdb-devel fuse3-devel openssl-devel pkg-config clang mold 2>/dev/null || \
            echo "    Some packages may not be in your repos - check manually"
    elif command -v apt-get &>/dev/null; then
        sudo apt-get install -y librocksdb-dev libfuse3-dev libssl-dev pkg-config clang mold
    fi
fi

# age + sops
for tool in age sops; do
    if ! command -v "${tool}" &>/dev/null; then
        echo "WARN: ${tool} not found. Install with:"
        echo "  Linux: sudo dnf install ${tool}  (or apt-get install ${tool})"
        echo "  macOS: brew install ${tool}"
    fi
done

echo ""
echo "==> Setup complete. Run: task --list"
