# Containerfile — tcfsd worker image
#
# Multi-stage build:
#   builder  — Rust release build with k8s-worker feature
#   runtime  — distroless/cc for minimal attack surface
#
# Build:
#   podman build -t ghcr.io/tummycrypt/tcfsd:latest -f Containerfile .
#
# Run:
#   podman run --rm \
#     --env-file /path/to/s3-credentials.env \
#     ghcr.io/tummycrypt/tcfsd:latest \
#     --mode=worker --config=/etc/tcfsd/config.toml

# ── Stage 1: Rust builder ─────────────────────────────────────────────────────

FROM rust:1.93-slim-bookworm AS builder

WORKDIR /build

# Install build deps for native crates (fuse3 headers, protobuf compiler)
RUN apt-get update -qq && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Cache dependency compilation: copy manifests first, then source
COPY Cargo.toml Cargo.lock ./
COPY crates/tcfs-core/Cargo.toml      crates/tcfs-core/
COPY crates/tcfs-secrets/Cargo.toml   crates/tcfs-secrets/
COPY crates/tcfs-storage/Cargo.toml   crates/tcfs-storage/
COPY crates/tcfs-chunks/Cargo.toml    crates/tcfs-chunks/
COPY crates/tcfs-sync/Cargo.toml      crates/tcfs-sync/
COPY crates/tcfs-fuse/Cargo.toml      crates/tcfs-fuse/
COPY crates/tcfs-cloudfilter/Cargo.toml crates/tcfs-cloudfilter/
COPY crates/tcfsd/Cargo.toml          crates/tcfsd/
COPY crates/tcfs-cli/Cargo.toml       crates/tcfs-cli/
COPY crates/tcfs-tui/Cargo.toml       crates/tcfs-tui/

# Create stub lib/main files so cargo can compute the dependency graph
RUN for d in tcfs-core tcfs-secrets tcfs-storage tcfs-chunks tcfs-sync tcfs-fuse tcfs-cloudfilter tcfs-tui; do \
      mkdir -p crates/$d/src && echo "// stub" > crates/$d/src/lib.rs; \
    done && \
    mkdir -p crates/tcfsd/src crates/tcfs-cli/src && \
    echo "fn main() {}" > crates/tcfsd/src/main.rs && \
    echo "fn main() {}" > crates/tcfs-cli/src/main.rs

# Build deps only (cached layer)
RUN cargo build --release --features tcfsd/k8s-worker -p tcfsd 2>&1 || true

# Copy real source
COPY crates/ crates/

# Build the worker binary
RUN touch crates/tcfsd/src/main.rs && \
    cargo build --release --features tcfsd/k8s-worker -p tcfsd

# ── Stage 2: Runtime (distroless) ─────────────────────────────────────────────

FROM gcr.io/distroless/cc-debian12:latest

# Copy binary
COPY --from=builder /build/target/release/tcfsd /tcfsd

# Default config location (override with -v or ConfigMap)
# Config must be mounted at /etc/tcfsd/config.toml
VOLUME ["/etc/tcfsd", "/var/lib/tcfsd"]

# Metrics port
EXPOSE 9100

# Graceful shutdown: SIGTERM is forwarded to tcfsd, which drains in-flight tasks
STOPSIGNAL SIGTERM

ENTRYPOINT ["/tcfsd"]
CMD ["--mode=worker", "--config=/etc/tcfsd/config.toml"]
