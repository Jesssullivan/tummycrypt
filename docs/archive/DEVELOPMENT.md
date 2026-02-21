# TummyCrypt Development Setup

This guide provides step-by-step instructions for setting up the TummyCrypt development environment.

## Prerequisites

- **OS**: Linux (Ubuntu 22.04+), macOS 12+, or Windows 11 with WSL2
- **RAM**: 8GB minimum, 16GB recommended
- **CPU**: 4+ cores
- **Storage**: 50GB available space
- **Podman**: For container orchestration (rootless)
- **GitLab Instance**: For OAuth device flow (or use GitLab.com)

## Quick Start

### 1. Clone Repository

```bash
git clone https://github.com/your-org/tummycrypt.git
cd tummycrypt
```

### 2. Install Development Tools

```bash
bash scripts/setup-dev.sh
```

This will install:
- Go 1.23
- Chapel 1.33
- Podman
- Protocol Buffers
- GoReleaser
- Task

### 3. Source Environment Variables

Add to your shell profile (`~/.bashrc` or `~/.zshrc`):

```bash
export PATH="/usr/local/go/bin:${PATH}"
export CHPL_HOME=/opt/chapel
export PATH="${CHPL_HOME}/bin:${PATH}"
```

Then reload:

```bash
source ~/.bashrc
```

### 4. Generate TLS Certificates

```bash
bash scripts/generate-certs.sh
```

This generates:
- Certificate Authority: `certs/SeaweedFS_CA.{crt,key}`
- Component certificates for: master-{1,2,3}, volume, filer, s3

### 5. Start Local Development Stack

```bash
task start-stack
```

This starts:
- 3 SeaweedFS master nodes (replicated)
- 1 volume server
- 1 filer node
- 1 S3 gateway

### 6. Build Components

```bash
task build-all
```

This builds:
- Go daemon: `./bin/tummycryptd`
- Chapel engine: `./bin/chapel_engine`

### 7. Run Tests

```bash
task test
```

This runs:
- Go unit and integration tests
- Chapel property-based tests

## Development Workflow

### Build

```bash
# Build Go daemon only
task build

# Build Chapel engine only
task build-chapel

# Build all components
task build-all

# Build with debug symbols
cd chapel
make debug
```

### Test

```bash
# Run all tests
task test

# Run Go tests only
task test-go

# Run Chapel tests only
task test-chapel

# Run unit tests only (skip integration)
task test-unit

# Run with coverage
task coverage
```

### Development Mode

```bash
# Run Go daemon with hot reload (requires `air`)
task dev

# Run daemon binary directly
task dev-daemon

# Run in debug mode
./bin/tummycryptd daemon --config config/config.yaml --debug
```

### Infrastructure

```bash
# Start local stack
task start-stack

# Stop local stack
task stop-stack

# Restart local stack
task restart-stack

# View logs
task logs

# View specific service logs
podman logs -f seaweed-filer
```

### Linting and Formatting

```bash
# Run linters
task lint

# Format Go code
go fmt ./...

# Format Chapel code (manual, Chapel has no auto-formatter)
```

### Release

```bash
# Create release binaries
task release

# Create snapshot release
task release-snapshot
```

## Local Stack Endpoints

Once started, the following endpoints are available:

| Service | Endpoint | Description |
|---------|-----------|-------------|
| Master 1 API | http://localhost:9333 | Master HTTP API |
| Master 1 gRPC | http://localhost:19333 | Master gRPC |
| Master 2 API | http://localhost:9334 | Master HTTP API |
| Master 3 API | http://localhost:9335 | Master HTTP API |
| Volume API | http://localhost:8080 | Volume HTTP API |
| Volume gRPC | http://localhost:18080 | Volume gRPC |
| Filer API | http://localhost:8888 | Filer HTTP API |
| Filer gRPC | http://localhost:18888 | Filer gRPC |
| S3 API | http://localhost:8333 | S3-compatible API |

## Testing SeaweedFS Stack

### Test Master

```bash
curl http://localhost:9333/cluster/status
```

### Test Volume

```bash
curl http://localhost:8080/status
```

### Test Filer

```bash
curl http://localhost:8888/
```

### Test S3 with AWS CLI

```bash
export AWS_ACCESS_KEY_ID=admin
# Also export the S3 secret credential (see docker-compose.yml for dev value)
export AWS_ENDPOINT_URL=http://localhost:8333

# List buckets
aws s3 ls --endpoint-url http://localhost:8333

# Create bucket
aws s3 mb s3://test-bucket --endpoint-url http://localhost:8333

# Upload file
echo "test" | aws s3 cp - s3://test-bucket/test.txt --endpoint-url http://localhost:8333
```

## Configuration

### Environment Variables

Create `.env.local`:

```bash
# TummyCrypt
TUMMYCRYPT_CONFIG=./config/config.yaml
TUMMYCRYPT_DATA_DIR=./data
TUMMYCRYPT_LOG_LEVEL=info
TUMMYCRYPT_DEBUG=true

# SeaweedFS
SEAWEEDFS_FILER_ENDPOINT=http://seaweed-filer:8888
SEAWEEDFS_S3_ENDPOINT=http://seaweed-s3:8333
SEAWEEDFS_ACCESS_KEY=admin
# SEAWEEDFS secret credential also set here (dev value: admin)

# GitLab OAuth
GITLAB_INSTANCE=https://gitlab.example.com
GITLAB_CLIENT_ID=your-client-id
GITLAB_CLIENT_SECRET=your-client-secret
GITLAB_REDIRECT_URI=http://localhost:8080/oauth/callback

# Chapel
CHPL_RT_NUM_THREADS_PER_LOCALE=4
dataParMaxChunkSize=65536
```

### Load Environment Variables

```bash
# Load into current shell
export $(cat .env.local | xargs)

# Or use direnv (recommended)
echo "dotenv .env.local" > .envrc
```

## GitLab OAuth Setup

### 1. Register Application

1. Navigate to: GitLab → User Settings → Applications
2. Create new application:
   - Name: `TummyCrypt`
   - Redirect URI: `http://localhost:8080/oauth/callback`
   - Scopes: `api`, `read_api`, `read_user`
3. Note:
   - Application ID → `GITLAB_CLIENT_ID`
   - Application Secret → `GITLAB_CLIENT_SECRET`

### 2. Update .env.local

```bash
GITLAB_CLIENT_ID="your-app-id"
GITLAB_CLIENT_SECRET="your-app-secret"
```

### 3. Test OAuth Flow

```bash
./bin/tummycryptd auth --verbose
```

## Troubleshooting

### SeaweedFS Won't Start

```bash
# Check logs
podman logs seaweed-master-1
podman logs seaweed-filer

# Verify network
podman network inspect seaweed-net

# Check certificates
openssl x509 -in certs/master-1.crt -text -noout

# Restart services
task restart-stack
```

### Chapel Build Failures

```bash
# Check Chapel version
chpl --version

# Verify library paths
echo $CHPL_HOME

# Check for missing dependencies
cd chapel
make debug

# Clean and rebuild
make clean
make build
```

### Go Build Failures

```bash
# Check Go version
go version

# Verify dependencies
go mod download
go mod verify

# Clean build cache
go clean -cache
task clean
task build
```

### Certificate Errors

```bash
# Regenerate certificates
task generate-certs

# Verify certificate chain
openssl verify -CAfile certs/SeaweedFS_CA.crt certs/master-1.crt

# Check expiration
openssl x509 -in certs/master-1.crt -noout -dates
```

### Port Conflicts

If ports are already in use:

```bash
# Check what's using ports
sudo lsof -i :9333
sudo lsof -i :8888

# Or modify ports in docker-compose.yml
```

## Development Tips

### Use Task

Task is more flexible than Makefile and provides better output:

```bash
# List all tasks
task

# Run task with arguments
task build --version=1.2.3

# Run tasks in parallel
task: parallel build build-chapel
```

### Hot Reload

Use `air` for Go development with hot reload:

```bash
# Install air
go install github.com/cosmtrek/air@latest

# Run in dev mode
task dev
```

### Debug Logging

Enable debug logging for detailed output:

```bash
# Environment variable
export TUMMYCRYPT_DEBUG=true

# Command line flag
./bin/tummycryptd daemon --debug

# Config file
# Set logging.level: "debug" in config.yaml
```

### Chapel Performance Tuning

Tune Chapel parallelism for your hardware:

```bash
# Set number of threads per locale
export CHPL_RT_NUM_THREADS_PER_LOCALE=8

# Set chunk size for data parallelism
export dataParMaxChunkSize=131072  # 128KB

# Set number of locales (for multi-locale)
export CHPL_LOCALE=0,1,2,3  # 4 locales
```

### Go Profiling

Profile Go code for performance analysis:

```bash
# CPU profile
go tool pprof http://localhost:9090/debug/pprof/profile

# Memory profile
go tool pprof http://localhost:9090/debug/pprof/heap

# Goroutine profile
go tool pprof http://localhost:9090/debug/pprof/goroutine
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on:
- Code style
- Commit messages
- Pull request process
- Testing requirements

## Additional Resources

- [CRUSH.md](CRUSH.md) - Complete design specification
- [AGENTS.md](AGENTS.md) - Documentation for agents
- [RETOOL.md](RETOOL.md) - Retooling goals
- [RESEARCH.md](RESEARCH.md) - Research findings
- [Ansible Deployment](README.md) - Original Ansible deployment

## Support

For issues or questions:
- GitHub Issues: https://github.com/your-org/tummycrypt/issues
- Documentation: https://docs.tummycrypt.io
- Slack: #tummycrypt-dev
