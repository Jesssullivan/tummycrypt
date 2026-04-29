# tcfs project Justfile
# Run `just --list` to see all recipes
# cargo is expected to come from the pinned rust-toolchain or Nix devShell.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Default: list recipes
default:
    @just --list

# ── Infrastructure ──────────────────────────────────────────────────────────

# Initialize OpenTofu for an environment
tofu-init env="civo":
    cd infra/tofu/environments/{{env}} && tofu init

# Plan OpenTofu changes
tofu-plan env="civo":
    cd infra/tofu/environments/{{env}} && tofu plan

# Apply OpenTofu changes
tofu-apply env="civo":
    cd infra/tofu/environments/{{env}} && tofu apply

# Validate OpenTofu configuration
tofu-validate env="civo":
    cd infra/tofu/environments/{{env}} && tofu validate

# ── Kubernetes ──────────────────────────────────────────────────────────────

# Show pod and service status
k8s-status ns="tcfs":
    kubectl get pods -n {{ns}}
    @echo "---"
    kubectl get svc -n {{ns}}

# Read-only on-prem authority/mobility check for tcfs
onprem-preflight:
    bash scripts/tcfs-onprem-preflight.sh

# Read-only on-prem data inventory for NATS and SeaweedFS migration planning
onprem-data-inventory:
    bash scripts/tcfs-onprem-data-inventory.sh

# Render non-mutating downtime migration facts, import Pods, or copy commands
onprem-migration-plan *ARGS:
    bash scripts/tcfs-onprem-migration-plan.sh {{ARGS}}

# Regression test the non-mutating TCFS migration command renderer
onprem-migration-plan-test:
    bash scripts/test-tcfs-onprem-migration-plan.sh

# Validate on-prem OpenTofu migration surfaces without applying live changes
onprem-tofu-validate:
    bash scripts/tcfs-onprem-tofu-validate.sh

# Tail logs from a workload
k8s-logs app="tcfsd" ns="tcfs":
    kubectl logs -l app.kubernetes.io/name={{app}} -n {{ns}} --tail=50

# Describe a pod (for debugging)
k8s-describe app="tcfsd" ns="tcfs":
    kubectl describe pods -l app.kubernetes.io/name={{app}} -n {{ns}}

# ── DNS ────────────────────────────────────────────────────────────────────

# Show current DNS records for tummycrypt.dev
dns-status:
    @echo "NATS Tailscale IP:"
    @kubectl get svc nats-tailscale -n tcfs -o jsonpath='{.status.loadBalancer.ingress[?(@.ip)].ip}'
    @echo ""
    @echo "DNS record:"
    @dig +short nats.tcfs.tummycrypt.dev

# Full deploy: infra + DNS (may need two runs for Tailscale IP)
# Fresh cluster (no CRDs installed yet):
#   just deploy-fresh   # Installs operators without CRD consumers
#   just deploy         # Second run creates ServiceMonitors + ScaledObject
# Import existing DNS record first if needed:
#   cd infra/tofu/environments/civo && tofu import 'module.nats_dns[0].porkbun_dns_record.this' 'tummycrypt.dev:RECORD_ID'
deploy env="civo":
    cd infra/tofu/environments/{{env}} && tofu apply

# First-apply on fresh cluster: skip CRD consumers (ServiceMonitor, ScaledObject)
deploy-fresh env="civo":
    cd infra/tofu/environments/{{env}} && tofu apply -var='enable_crds=false'

# ── NATS ────────────────────────────────────────────────────────────────────

# Check NATS server info via Tailscale
nats-status server="nats://nats.tcfs.tummycrypt.dev:4222":
    nats server info --server {{server}}

# List JetStream streams
nats-streams server="nats://nats.tcfs.tummycrypt.dev:4222":
    nats stream ls --server {{server}}

# Publish a test ping to verify connectivity
nats-ping server="nats://nats.tcfs.tummycrypt.dev:4222":
    @echo "Pinging NATS via Tailscale..."
    nats pub STATE.ping '{"from":"operator","ts":"'$(date -Iseconds)'"}' --server {{server}}

# ── Fleet ───────────────────────────────────────────────────────────────────

# Check fleet NATS connectivity from this machine
fleet-check:
    @echo "Checking fleet NATS connectivity..."
    nats server info --server nats://nats.tcfs.tummycrypt.dev:4222

# Canonical live fleet acceptance lane: SeaweedFS + NATS + neo↔honey sync path
neo-honey-smoke:
    bash scripts/neo-honey-smoke.sh

# Installed-binary smoke for release surfaces that ship tcfsd (and optionally tcfs)
install-smoke *ARGS:
    bash scripts/install-smoke.sh {{ARGS}}

# ── Nix ─────────────────────────────────────────────────────────────────────

# Build tcfsd via Nix
nix-build:
    nix build .#tcfsd

# Run nix flake check
nix-check:
    nix flake check

# Enter the dev shell
nix-devshell:
    nix develop

# Show active toolchain and local environment helpers
toolchain-status:
    rustc --version
    cargo --version
    just --version
    @if command -v nix >/dev/null 2>&1; then nix --version; else echo "nix: not installed"; fi
    @if command -v direnv >/dev/null 2>&1; then direnv version; else echo "direnv: not installed"; fi

# ── Cargo ───────────────────────────────────────────────────────────────────

# Build workspace
build:
    cargo build --workspace

# Run all tests
test:
    cargo test --workspace

# Lint (clippy + fmt check)
lint:
    cargo clippy --workspace --all-targets
    cargo fmt --all -- --check

# cargo-deny license and advisory check
deny:
    cargo deny check

# ── iOS (TestFlight) ─────────────────────────────────────────────────────
# Pipeline: Rust staticlib → xcodegen → archive → export IPA → TestFlight
# All ios-* recipes must run on PZM from Terminal.app (keychain access).

# Full iOS release: build Rust, archive, export, upload
ios-release:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "==> TCFS iOS Release Pipeline"
    echo ""
    echo "==> [1/3] Rust staticlib + UniFFI..."
    nix develop -c bash swift/ios/Scripts/build-rust.sh --clean
    echo ""
    echo "==> [2/3] Xcode archive + export..."
    bash swift/ios/Scripts/xcode-pipeline.sh
    echo ""
    echo "==> [3/3] TestFlight upload..."
    bash swift/ios/Scripts/upload.sh
    echo ""
    echo "==> Release pipeline complete!"

# Build Rust staticlib + UniFFI bindings (inside nix devshell)
ios-rust *FLAGS:
    nix develop -c bash swift/ios/Scripts/build-rust.sh {{FLAGS}}

# Archive + export IPA (must NOT be inside nix devshell)
ios-archive *FLAGS:
    bash swift/ios/Scripts/xcode-pipeline.sh {{FLAGS}}

# Export from existing archive + upload (resume after archive succeeded)
ios-upload:
    bash swift/ios/Scripts/xcode-pipeline.sh --skip-archive
    bash swift/ios/Scripts/upload.sh

# Upload an existing IPA to TestFlight
ios-upload-ipa ipa="swift/ios/build/export/TCFS.ipa":
    bash swift/ios/Scripts/upload.sh {{ipa}}

# Setup signing certs (one-time, from Terminal.app)
ios-signing:
    bash swift/ios/Scripts/setup-signing.sh

# Clean iOS build artifacts
ios-clean:
    rm -rf swift/ios/build swift/ios/TCFS.xcodeproj

# Clean everything including Rust iOS target
ios-clean-all: ios-clean
    rm -rf target/aarch64-apple-ios

# ── FileProvider (Darwin) ──────────────────────────────────────────────────

# Build FileProvider .appex bundle (macOS only, requires Xcode CLT)
fileprovider-build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$(uname)" != "Darwin" ]; then
        echo "FileProvider extension only builds on macOS" >&2
        exit 1
    fi
    echo "==> Building Rust staticlib..."
    cargo build -p tcfs-file-provider --release -j 4
    HEADER=$(find target/release/build -name "tcfs_file_provider.h" | head -1)
    if [ -z "$HEADER" ]; then
        echo "ERROR: cbindgen header not found" >&2
        exit 1
    fi
    echo "==> Building Swift .appex bundle..."
    bash swift/fileprovider/build.sh "target/release" "$HEADER" "build/fileprovider"
    echo "==> Output: build/fileprovider/TCFSProvider.app"

# Provision FileProvider config (writes S3 creds to App Group container)
fileprovider-provision:
    bash swift/fileprovider/provision-config.sh

# Install FileProvider .appex to ~/Applications (macOS only)
fileprovider-install: fileprovider-build fileprovider-provision
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p ~/Applications
    rm -rf ~/Applications/TCFSProvider.app
    cp -R build/fileprovider/TCFSProvider.app ~/Applications/
    echo "==> Installed to ~/Applications/TCFSProvider.app"
    echo "==> Launching to register FileProvider domain..."
    open ~/Applications/TCFSProvider.app
