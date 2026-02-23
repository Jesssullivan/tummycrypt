# tcfs project Justfile
# Run `just --list` to see all recipes

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
    @kubectl get svc nats-tailscale -n tcfs -o jsonpath='{.status.loadBalancer.ingress[0].ip}'
    @echo ""
    @echo "DNS record:"
    @dig +short nats.tcfs.tummycrypt.dev

# Full deploy: infra + DNS (may need two runs for Tailscale IP)
deploy env="civo":
    cd infra/tofu/environments/{{env}} && tofu apply

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

# ── Cargo ───────────────────────────────────────────────────────────────────

# Build workspace
build:
    ~/.cargo/bin/cargo build --workspace

# Run all tests
test:
    ~/.cargo/bin/cargo test --workspace

# Lint (clippy + fmt check)
lint:
    ~/.cargo/bin/cargo clippy --workspace --all-targets
    ~/.cargo/bin/cargo fmt --all -- --check

# cargo-deny license and advisory check
deny:
    ~/.cargo/bin/cargo deny check
