# tcfs project Justfile
# Run `just --list` to see all recipes
# cargo is expected to come from the pinned rust-toolchain or Nix devShell.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Default: list recipes
default:
    @just --list

# ── Infrastructure ──────────────────────────────────────────────────────────

# Initialize OpenTofu for an environment (defaults to current on-prem)
tofu-init env="onprem":
    cd infra/tofu/environments/{{env}} && tofu init

# Plan OpenTofu changes
tofu-plan env="onprem":
    cd infra/tofu/environments/{{env}} && tofu plan

# Apply OpenTofu changes (requires explicit env)
tofu-apply env="":
    @test -n "{{env}}" || (echo "pass env=onprem, env=local, or env=civo explicitly" >&2; exit 2)
    cd infra/tofu/environments/{{env}} && tofu apply

# Validate OpenTofu configuration
tofu-validate env="onprem":
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

# Regression test the read-only on-prem authority/mobility check
onprem-preflight-test:
    bash scripts/test-tcfs-onprem-preflight.sh

# Read-only on-prem data inventory for NATS and SeaweedFS migration planning
onprem-data-inventory:
    bash scripts/tcfs-onprem-data-inventory.sh

# Render non-mutating downtime migration facts, import Pods, or copy commands
onprem-migration-plan *ARGS:
    bash scripts/tcfs-onprem-migration-plan.sh {{ARGS}}

# Render the non-mutating downtime cutover packet after window/owners are named
onprem-cutover-packet:
    bash scripts/tcfs-onprem-cutover-packet.sh

# Regression test the non-mutating TCFS cutover packet renderer
onprem-cutover-packet-test:
    bash scripts/test-tcfs-onprem-cutover-packet.sh

# Regression test the non-mutating TCFS migration command renderer
onprem-migration-plan-test:
    bash scripts/test-tcfs-onprem-migration-plan.sh

# Validate on-prem OpenTofu migration surfaces without applying live changes
onprem-tofu-validate:
    bash scripts/tcfs-onprem-tofu-validate.sh

# Static safety checks for source-owned on-prem candidate workload selectors
onprem-tofu-candidate-test:
    bash scripts/test-tcfs-onprem-tofu-candidate-workloads.sh

# Tail logs from a workload
k8s-logs app="tcfsd" ns="tcfs":
    kubectl logs -l app.kubernetes.io/name={{app}} -n {{ns}} --tail=50

# Describe a pod (for debugging)
k8s-describe app="tcfsd" ns="tcfs":
    kubectl describe pods -l app.kubernetes.io/name={{app}} -n {{ns}}

# ── DNS ────────────────────────────────────────────────────────────────────

# Show current legacy/standby Civo DNS records for tummycrypt.dev
dns-status:
    @echo "NATS Tailscale IP:"
    @kubectl get svc nats-tailscale -n tcfs -o jsonpath='{.status.loadBalancer.ingress[?(@.ip)].ip}'
    @echo ""
    @echo "DNS record:"
    @dig +short nats.tcfs.tummycrypt.dev

# Fresh cluster (no CRDs installed yet):
#   just deploy-fresh   # Installs operators without CRD consumers
#   just deploy         # Second run creates ServiceMonitors + ScaledObject
# Import existing DNS record first if needed:
#   cd infra/tofu/environments/civo && tofu import 'module.nats_dns[0].porkbun_dns_record.this' 'tummycrypt.dev:RECORD_ID'
# Full deploy: infra + DNS (legacy civo-era helper; requires explicit env)
deploy env="":
    @test -n "{{env}}" || (echo "pass env=onprem, env=local, or env=civo explicitly" >&2; exit 2)
    cd infra/tofu/environments/{{env}} && tofu apply

# First-apply on fresh cluster: skip CRD consumers (ServiceMonitor, ScaledObject)
deploy-fresh env="":
    @test -n "{{env}}" || (echo "pass env=onprem, env=local, or env=civo explicitly" >&2; exit 2)
    cd infra/tofu/environments/{{env}} && tofu apply -var='enable_crds=false'

# ── NATS ────────────────────────────────────────────────────────────────────

# Check legacy/standby Civo NATS server info via Tailscale
nats-status server="nats://nats.tcfs.tummycrypt.dev:4222":
    nats server info --server {{server}}

# List legacy/standby Civo JetStream streams
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

# Regression test the neo/honey evidence-packet wrapper without live services
neo-honey-smoke-test:
    bash scripts/test-neo-honey-smoke.sh

# Read-only alpha productionization gate classifier
alpha-gate-preflight *ARGS:
    @bash scripts/tcfs-alpha-gate-preflight.sh {{ARGS}}

# Regression test the alpha productionization gate classifier
alpha-gate-preflight-test:
    @bash scripts/test-tcfs-alpha-gate-preflight.sh

# Static regression test for the Linux package post-install smoke workflow
linux-postinstall-workflow-test:
    @bash scripts/test-linux-postinstall-workflow.sh

# Static regression test for the Linux package container smoke workflow
linux-package-container-workflow-test:
    @bash scripts/test-linux-package-container-smoke-workflow.sh

# Static regression test for the storage large restore canary workflow
storage-large-restore-workflow-test:
    @bash scripts/test-storage-large-restore-canary-workflow.sh

# Regression test the storage large restore SLO evaluator
storage-large-restore-slo-test:
    @bash scripts/test-evaluate-storage-large-restore-slo.sh

# Read-only inventory packet for a candidate large workdir
large-workdir-inventory ROOT *ARGS:
    python3 scripts/large-workdir-inventory.py {{ROOT}} {{ARGS}}

# Regression test the large workdir inventory helper
large-workdir-inventory-test:
    python3 scripts/test-large-workdir-inventory.py

# Installed-binary smoke for release surfaces that ship tcfsd (and optionally tcfs)
install-smoke *ARGS:
    bash scripts/install-smoke.sh {{ARGS}}

# Regression test the installed-binary smoke helper with fake installed binaries
install-smoke-test:
    bash scripts/test-install-smoke.sh

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
