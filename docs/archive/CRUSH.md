# TummyCrypt Design Document

> **Last Updated**: 2025-01-01
> **Status**: Design Phase - Research Complete, Ready for Implementation
> **Directive**: This document is a living specification. Update requirements and patterns as implementation proceeds.

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Architecture Overview](#architecture-overview)
3. [Component Specification](#component-specification)
4. [Development Environment](#development-environment)
5. [Build & Compilation](#build--compilation)
6. [Testing Strategy](#testing-strategy)
7. [Security Architecture](#security-architecture)
8. [Infrastructure Requirements](#infrastructure-requirements)
9. [Deployment Patterns](#deployment-patterns)
10. [Code Patterns & Best Practices](#code-patterns--best-practices)
11. [Development Cycle](#development-cycle)

---

## Executive Summary

TummyCrypt is transforming from an Ansible-based SeaweedFS deployment into a modern, secure S3 synchronization and backup system using:

- **Go 1.23+**: System daemon (I/O-bound, network operations, TPM attestation)
- **Chapel 1.33+**: Parallel compute engine (file diffing, checksums, chunk processing)
- **Crush Patterns**: KDBX + GitLab SecureEnclave authentication, Cobra CLI with Bubbletea TUI
- **SeaweedFS Native API**: Direct filer integration for optimal performance

### Key Design Decisions

| Decision | Rationale | Pattern Source |
|----------|-----------|----------------|
| Hybrid Go/Chapel | Go for I/O/TPM, Chapel for parallelism | Crush auth patterns, blahaj/mail-api Chapel FFI |
| KDBX + GitLab OAuth | Proven secure credential management | Crush internal/secrets/ |
| Cobra + Bubbletea | Excellent CLI/UX patterns | Crush internal/cmd/, internal/tui/ |
| Podman Compose | Rootless container orchestration | tinyland.dev container patterns |
| Chapel QuickCheck | Property-based testing for sync algorithms | chapelCheck library |
| Native SeaweedFS API | More efficient than S3 compatibility layer | SeaweedFS GitHub repo |

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                     TummyCrypt Daemon (Go)                       │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────┐   │
│  │   CLI (Go)  │  │  Auth/KDBX   │  │   TPM Attestation   │   │
│  │  (Cobra+    │  │   Provider   │  │     Module          │   │
│  │  Bubbletea) │  │              │  │                     │   │
│  └──────┬──────┘  └──────┬───────┘  └──────────┬──────────┘   │
│         │                │                     │                 │
│         │                │                     │                 │
│  ┌──────▼────────────────▼─────────────────────▼──────────┐   │
│  │            Sync Engine (Go)                            │   │
│  │  - Conflict Resolution                                 │   │
│  │  - Delta Transfer                                      │   │
│  │  - Encryption/Decryption                               │   │
│  └──────┬──────────────────────────────────────────────────┘   │
└─────────┼──────────────────────────────────────────────────────┘
          │ Protocol Buffers IPC
┌─────────▼──────────────────────────────────────────────────────┐
│           Chapel Compute Engine                                │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────┐   │
│  │ File Diff   │  │ Checksum     │  │  Chunk Processing   │   │
│  │ (forall)    │  │ (forall)     │  │  (forall)           │   │
│  └─────────────┘  └──────────────┘  └─────────────────────┘   │
└─────────┬──────────────────────────────────────────────────────┘
          │
          │ SeaweedFS Native API (gRPC/HTTP)
┌─────────▼──────────────────────────────────────────────────────┐
│                    SeaweedFS Cluster                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │
│  │ Master 1 │  │ Master 2 │  │ Master 3 │  │ Volume Nodes │  │
│  │ (replica)│  │ (replica)│  │ (replica)│  │ (triplicated)│  │
│  └──────────┘  └──────────┘  └──────────┘  └──────┬───────┘  │
│                                                    │          │
│  ┌─────────────────────────────────────────────────▼──────────┐ │
│  │                    Filer Node                             │ │
│  │            (POSIX-like API + S3 Gateway)                  │ │
│  └──────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

### Data Flow

```
1. User Command (CLI)
   └─> TummyCrypt sync ~/mydata tummycrypt:/backup

2. Go Daemon: Authentication
   └─> KDBX keystore check
   └─> GitLab OAuth (if needed)
   └─> TPM attestation (device binding)

3. Go Daemon: Sync Planning
   └─> Request file list from Chapel via IPC
   └─> Chapel computes diffs with forall parallelism
   └─> Return delta to Go daemon

4. Go Daemon: Transfer
   └─> Encrypt chunks (AES-256-GCM)
   └─> Send to SeaweedFS filer via gRPC
   └─> Store metadata in LevelDB2

5. SeaweedFS: Storage
   └─> Filer distributes to volume servers
   └─> Triplicated storage (3 replicas)

6. Go Daemon: Verification
   └─> Request checksum verification from Chapel
   └─> Compare local vs remote checksums
   └─> Report status to CLI
```

---

## Component Specification

### 1. Go Daemon (tummycryptd)

**Responsibilities**:
- CLI interface (Cobra + Bubbletea TUI)
- Authentication (KDBX + GitLab OAuth + TPM)
- Sync orchestration
- SeaweedFS API integration
- IPC with Chapel engine

**Key Modules** (following Crush patterns):

```
cmd/
├── root.go              # Root command with Cobra
├── sync.go              # sync subcommand
├── status.go            # status subcommand
├── auth.go              # auth subcommand (GitLab OAuth)
├── init.go              # init subcommand (KDBX creation)
└── logs.go              # logs subcommand

internal/
├── daemon/
│   ├── daemon.go        # Main daemon lifecycle
│   └── config.go        # Configuration management
├── sync/
│   ├── engine.go        # Sync orchestration
│   ├── conflict.go      # Conflict resolution
│   └── delta.go         # Delta transfer logic
├── seaweedfs/
│   ├── client.go        # SeaweedFS filer gRPC client
│   ├── volume.go        # Volume operations
│   └── metadata.go      # Metadata cache
├── chapel/
│   ├── ipc.go           # Protocol Buffers IPC layer
│   ├── bridge.go        # Go-Chapel FFI bridge
│   └── pool.go          # Chapel process pool
├── secrets/
│   ├── provider/
│   │   └── provider.go  # Provider interface (from Crush)
│   ├── keystore/
│   │   └── kdbx.go     # KDBX implementation (from Crush)
│   ├── binding/
│   │   ├── tpm.go       # TPM 2.0 binding
│   │   └── enclave_darwin.go  # Secure Enclave (from Crush)
│   └── gitlab/
│       └── oauth.go     # GitLab device flow (from Crush)
├── crypto/
│   ├── encrypt.go       # AES-256-GCM encryption
│   └── keys.go          # Key management
└── tui/
    ├── tui.go           # Main TUI model (from Crush)
    ├── components/      # TUI components
    └── keys.go          # Key bindings
```

**KDBX Provider Interface** (from Crush pattern):

```go
// internal/secrets/provider/provider.go
package provider

import "context"

type Provider interface {
    Get(ctx context.Context, path string) (*Credential, error)
    Set(ctx context.Context, cred *Credential) error
    Delete(ctx context.Context, path string) error
    List(ctx context.Context, prefix string) ([]*Credential, error)
    Exists(ctx context.Context, path string) (bool, error)
    Lock() error
    IsLocked() bool
    Status() ProviderStatus
}

type Credential struct {
    Path     string   // e.g., "seaweedfs/filer/api_key"
    Value    string   // API key/token
    Category Category  // Service, Provider, Internal
    Metadata CredentialMetadata
}

type CredentialMetadata struct {
    CreatedAt      time.Time
    LastAccessed   time.Time
    RotationPolicy RotationPolicy
}

type Category int

const (
    CategoryService Category = iota  // SeaweedFS, GitLab, etc.
    CategoryProvider                 // OpenAI, Anthropic, etc.
    CategoryInternal                 // Encryption keys, JWT
)
```

**GitLab OAuth Device Flow** (from Crush pattern):

```go
// internal/secrets/gitlab/oauth.go
package gitlab

import (
    "context"
    "encoding/json"
    "fmt"
    "time"
)

type Client struct {
    clientID     string
    redirectURI  string
    authBaseURL  string
    tokenBaseURL string
    http         *http.Client
}

type DeviceAuthResponse struct {
    DeviceCode              string `json:"device_code"`
    UserCode               string `json:"user_code"`
    VerificationURI        string `json:"verification_uri"`
    VerificationURIComplete string `json:"verification_uri_complete"`
    ExpiresIn              int    `json:"expires_in"`
    Interval               int    `json:"interval"`
}

type TokenResponse struct {
    AccessToken  string `json:"access_token"`
    RefreshToken string `json:"refresh_token"`
    ExpiresIn    int    `json:"expires_in"`
    TokenType    string `json:"token_type"`
}

func (c *Client) StartDeviceFlow(ctx context.Context) (*DeviceAuthResponse, error) {
    url := fmt.Sprintf("%s/oauth/authorize_device", c.authBaseURL)
    req := map[string]string{
        "client_id":  c.clientID,
        "scope":      "read_api write_api",
        "redirect_uri": c.redirectURI,
    }

    resp, err := http.Post(url, "application/json", jsonReader(req))
    if err != nil {
        return nil, err
    }
    defer resp.Body.Close()

    var result DeviceAuthResponse
    if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
        return nil, err
    }

    return &result, nil
}

func (c *Client) PollForToken(ctx context.Context, deviceCode string, interval time.Duration) (*TokenResponse, error) {
    ticker := time.NewTicker(interval)
    defer ticker.Stop()

    for {
        select {
        case <-ctx.Done():
            return nil, ctx.Err()
        case <-ticker.C:
            url := fmt.Sprintf("%s/oauth/token", c.tokenBaseURL)
            req := map[string]string{
                "client_id":   c.clientID,
                "grant_type":  "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": deviceCode,
            }

            resp, err := http.Post(url, "application/json", jsonReader(req))
            if err != nil {
                continue
            }

            var result map[string]interface{}
            json.NewDecoder(resp.Body).Decode(&result)

            if err, ok := result["error"].(string); ok {
                if err == "authorization_pending" || err == "slow_down" {
                    continue
                }
                if err == "expired_token" {
                    return nil, fmt.Errorf("device code expired")
                }
                if err == "access_denied" {
                    return nil, fmt.Errorf("access denied by user")
                }
            }

            var token TokenResponse
            json.NewDecoder(resp.Body).Decode(&token)
            return &token, nil
        }
    }
}
```

**CLI Command Structure** (from Crush pattern):

```go
// cmd/root.go
package cmd

import (
    "github.com/spf13/cobra"
    "github.com/charmbracelet/bubbletea"
    "tummycrypt/internal/tui"
)

var rootCmd = &cobra.Command{
    Use:   "tummycrypt",
    Short: "Secure S3 synchronization with SeaweedFS",
    Long: `TummyCrypt is a secure synchronization and backup system
that uses SeaweedFS for distributed storage with end-to-end encryption.`,
    RunE: func(cmd *cobra.Command, args []string) error {
        // Interactive TUI mode
        app, err := setupApp(cmd)
        if err != nil {
            return err
        }
        defer app.Shutdown()

        ui := tui.New(app)
        program := tea.NewProgram(ui, tea.WithAltScreen())
        _, err = program.Run()
        return err
    },
}

func init() {
    rootCmd.PersistentFlags().StringP("config", "c", "", "Config file path")
    rootCmd.PersistentFlags().StringP("data-dir", "D", "", "Data directory")
    rootCmd.PersistentFlags().BoolP("debug", "d", false, "Debug mode")
    rootCmd.PersistentFlags().BoolP("verbose", "v", false, "Verbose output")

    rootCmd.AddCommand(syncCmd, statusCmd, authCmd, initCmd, logsCmd)
}

func Execute() {
    if err := rootCmd.Execute(); err != nil {
        os.Exit(1)
    }
}
```

**Sync Command**:

```go
// cmd/sync.go
package cmd

import (
    "context"
    "fmt"
    "time"

    "github.com/spf13/cobra"
)

var syncCmd = &cobra.Command{
    Use:   "sync <local-path> <remote-path>",
    Short: "Synchronize local directory with remote SeaweedFS",
    Example: `
  # Sync current directory
  tummycrypt sync . tummycrypt:/backup

  # Sync with verbose output
  tummycrypt sync -v ~/data tummycrypt:/backup
    `,
    Args: cobra.ExactArgs(2),
    RunE: func(cmd *cobra.Command, args []string) error {
        localPath := args[0]
        remotePath := args[1]

        verbose, _ := cmd.Flags().GetBool("verbose")
        dryRun, _ := cmd.Flags().GetBool("dry-run")

        ctx := context.Background()
        engine := sync.NewEngine(localPath, remotePath, verbose, dryRun)

        progress, err := engine.Run(ctx)
        if err != nil {
            return err
        }

        fmt.Printf("Sync complete: %d files transferred, %d skipped\n",
            progress.Transferred, progress.Skipped)
        return nil
    },
}

func init() {
    syncCmd.Flags().BoolP("verbose", "v", false, "Verbose output")
    syncCmd.Flags().Bool("dry-run", false, "Show what would be done without executing")
    syncCmd.Flags().String("delete", "never", "Delete policy: never, before, after")
    syncCmd.Flags().String("conflict", "newer", "Conflict resolution: newer, local, remote")
}
```

### 2. Chapel Compute Engine (chapel_engine)

**Responsibilities**:
- Parallel file diffing
- Checksum computation (SHA-256)
- Chunk processing
- Performance-critical algorithms

**Module Structure** (from blahaj/mail-api patterns):

```
chapel/
├── Main.chpl                   # Entry point, FFI export
├── Config.chpl                 # Environment configuration
├── Logger.chpl                 # Structured logging
├── Diff.chpl                   # Parallel file diffing
├── Checksum.chpl               # Checksum computation
├── Chunk.chpl                  # Chunk processing
├── Bridge.chpl                 # Go-Chapel FFI bridge
└── tests/
    ├── TestDiff.chpl           # Property-based tests
    ├── TestChecksum.chpl
    ├── TestChunk.chpl
    └── TestBridge.chpl
```

**Parallel File Diffing** (Chapel forall pattern):

```chapel
// Diff.chpl
module Diff {
    use Time;

    config const numWorkers = here.maxTaskPar;
    config const chunkSize = 64 * 1024;  // 64KB chunks

    record FileDiff {
        var localPath: string;
        var remotePath: string;
        var added: list(string);
        var modified: list(string);
        var deleted: list(string);
        var unchanged: list(string);
    }

    proc computeDiff(localPath: string, remotePath: string): FileDiff throws {
        var diff: FileDiff;
        diff.localPath = localPath;
        diff.remotePath = remotePath;

        // Get local file list
        var localFiles = scanDirectory(localPath);

        // Get remote file list (via Go IPC)
        var remoteFiles = getRemoteFileList(remotePath);

        // Parallel comparison
        forall (lf, rf) in zip(localFiles, remoteFiles) {
            var localChecksum = computeChecksum(lf);
            var remoteChecksum = getRemoteChecksum(rf);

            if localChecksum != remoteChecksum {
                diff.modified.pushBack(lf);
            } else {
                diff.unchanged.pushBack(lf);
            }
        }

        // Check for added/deleted
        // ... (omitted for brevity)

        return diff;
    }

    // Parallel checksum computation
    proc computeChecksum(path: string): string throws {
        var f = open(path, ioMode.r);
        var reader = f.reader(locking=false);
        defer f.close();

        var buffer: [0..chunkSize-1] uint(8);
        var checksums: list(string);

        while reader.read(buffer) {
            var hash = sha256(buffer);
            checksums.pushBack(hash);
        }

        // Combine checksums
        return combineChecksums(checksums);
    }
}
```

**Go-Chapel FFI Bridge**:

```chapel
// Bridge.chpl
module Bridge {
    require "bridge.h";

    use CTypes;
    use Diff;
    use Checksum;

    // Callback function pointers
    extern type DiffCallback = c_fn_callback(c_ptr(c_char), c_ptr(c_char), c_ptr(Diff));

    // Exported function for Go to call
    export proc chapelComputeDiff(
        localPath: c_ptr(c_char),
        remotePath: c_ptr(c_char),
        callback: DiffCallback,
        userData: c_ptr(void)
    ) {
        var local = string.createCopyingBuffer(localPath);
        var remote = string.createCopyingBuffer(remotePath);

        var diff = computeDiff(local, remote);

        // Invoke Go callback with result
        callback(localPath, remotePath, c_ptrTo(diff), userData);
    }

    export proc chapelComputeChecksum(
        path: c_ptr(c_char)
    ): c_ptr(c_char) {
        var p = string.createCopyingBuffer(path);
        var checksum = computeChecksum(p);
        return checksum.c_str();
    }
}
```

### 3. Go-Chapel IPC Protocol

**Protocol Buffer Definition**:

```protobuf
// ipc/tummycrypt.proto
syntax = "proto3";

package tummycrypt.ipc;

service ComputeEngine {
    rpc ComputeDiff(DiffRequest) returns (DiffResponse);
    rpc ComputeChecksum(ChecksumRequest) returns (ChecksumResponse);
    rpc ProcessChunks(ChunkRequest) returns (ChunkResponse);
}

message DiffRequest {
    string local_path = 1;
    string remote_path = 2;
    bool recursive = 3;
    repeated string exclude_patterns = 4;
}

message DiffResponse {
    repeated FileDiff added_files = 1;
    repeated FileDiff modified_files = 2;
    repeated FileDiff deleted_files = 3;
    repeated FileDiff unchanged_files = 4;
    int64 compute_time_ms = 5;
}

message FileDiff {
    string path = 1;
    string local_checksum = 2;
    string remote_checksum = 3;
    int64 size_bytes = 4;
    int64 modified_time_secs = 5;
}

message ChecksumRequest {
    repeated string paths = 1;
    string algorithm = 2;  // sha256, md5, etc.
}

message ChecksumResponse {
    map<string, string> checksums = 1;
}

message ChunkRequest {
    string file_path = 1;
    int64 offset = 2;
    int64 size = 3;
    int32 chunk_count = 4;
}

message ChunkResponse {
    repeated Chunk chunks = 1;
}

message Chunk {
    bytes data = 1;
    string checksum = 2;
}
```

---

## Development Environment

### Local Development Stack

**Podman Compose Services** (from tinyland.dev patterns):

```yaml
# docker-compose.yml
version: '3.8'

services:
  # SeaweedFS Master (replicated 3x)
  seaweed-master-1:
    image: chrislusf/seaweedfs:latest
    container_name: seaweed-master-1
    ports:
      - "9333:9333"   # HTTP API
      - "19333:19333" # gRPC
    command: >
      master
      -ip=seaweed-master-1
      -ip.bind=0.0.0.0
      -peers=seaweed-master-1:9333,seaweed-master-2:9333,seaweed-master-3:9333
      -volumeSizeLimitMB=10000
    volumes:
      - master_data_1:/data
      - ./certs:/certs:ro
    networks:
      - seaweed-net
    healthcheck:
      test: ["CMD", "wget", "--spider", "-q", "http://localhost:9333/cluster/status"]
      interval: 10s
      timeout: 5s
      retries: 3
      start_period: 10s

  seaweed-master-2:
    image: chrislusf/seaweedfs:latest
    container_name: seaweed-master-2
    ports:
      - "9334:9333"
      - "19334:19333"
    command: >
      master
      -ip=seaweed-master-2
      -ip.bind=0.0.0.0
      -peers=seaweed-master-1:9333,seaweed-master-2:9333,seaweed-master-3:9333
    volumes:
      - master_data_2:/data
      - ./certs:/certs:ro
    networks:
      - seaweed-net
    depends_on:
      - seaweed-master-1

  seaweed-master-3:
    image: chrislusf/seaweedfs:latest
    container_name: seaweed-master-3
    ports:
      - "9335:9333"
      - "19335:19333"
    command: >
      master
      -ip=seaweed-master-3
      -ip.bind=0.0.0.0
      -peers=seaweed-master-1:9333,seaweed-master-2:9333,seaweed-master-3:9333
    volumes:
      - master_data_3:/data
      - ./certs:/certs:ro
    networks:
      - seaweed-net
    depends_on:
      - seaweed-master-1

  # Volume Server
  seaweed-volume:
    image: chrislusf/seaweedfs:latest
    container_name: seaweed-volume
    ports:
      - "8080:8080"   # HTTP
      - "18080:18080" # gRPC
    command: >
      volume
      -mserver=seaweed-master-1:9333,seaweed-master-2:9333,seaweed-master-3:9333
      -port=8080
      -dir=/data
      -dataCenter=dc1
      -rack=dc1-rack1
    volumes:
      - volume_data:/data
      - ./certs:/certs:ro
    networks:
      - seaweed-net
    depends_on:
      - seaweed-master-1
      - seaweed-master-2
      - seaweed-master-3
    healthcheck:
      test: ["CMD", "wget", "--spider", "-q", "http://localhost:8080/status"]
      interval: 10s
      timeout: 5s
      retries: 3

  # Filer with S3 Gateway
  seaweed-filer:
    image: chrislusf/seaweedfs:latest
    container_name: seaweed-filer
    ports:
      - "8888:8888"   # Filer HTTP API
      - "18888:18888" # Filer gRPC
      - "8333:8333"   # S3 API
    command: >
      filer
      -master=seaweed-master-1:9333,seaweed-master-2:9333,seaweed-master-3:9333
      -ip=seaweed-filer
      -ip.bind=0.0.0.0
    volumes:
      - filer_data:/data
      - ./config/filer.toml:/etc/seaweedfs/filer.toml:ro
      - ./certs:/certs:ro
    networks:
      - seaweed-net
    depends_on:
      - seaweed-volume
    healthcheck:
      test: ["CMD", "wget", "--spider", "-q", "http://localhost:8888/"]
      interval: 10s
      timeout: 5s
      retries: 3

  # S3 Gateway
  seaweed-s3:
    image: chrislusf/seaweedfs:latest
    container_name: seaweed-s3
    ports:
      - "8333:8333"   # S3 API
    command: >
      s3
      -filer=seaweed-filer:8888
      -domainName=s3.local
      -key.file=/etc/seaweedfs/s3.json
    volumes:
      - ./config/s3.json:/etc/seaweedfs/s3.json:ro
      - ./certs:/certs:ro
    networks:
      - seaweed-net
    depends_on:
      - seaweed-filer

  # MCP Development Stack
  mcp-stack:
    build:
      context: .
      dockerfile: Containerfile.mcp
    container_name: tummycrypt-mcp
    volumes:
      - .:/workspace:z
      - mcp_data:/mcp-data
      - /var/run/podman/podman.sock:/var/run/podman/podman.sock:ro
    environment:
      - MCP_CREDENTIAL_PATH=/mcp-data/credentials
      - SEAWEEDFS_ENDPOINT=http://seaweed-filer:8888
      - SEAWEEDFS_S3_ENDPOINT=http://seaweed-s3:8333
      - GITLAB_INSTANCE=https://gitlab.example.com
    networks:
      - seaweed-net
    profiles:
      - mcp

volumes:
  master_data_1:
    driver: local
  master_data_2:
    driver: local
  master_data_3:
    driver: local
  volume_data:
    driver: local
  filer_data:
    driver: local
  mcp_data:
    driver: local

networks:
  seaweed-net:
    driver: bridge
```

**Filer Configuration** (from SeaweedFS patterns):

```toml
# config/filer.toml

# LevelDB2 for metadata storage
[leveldb2]
enabled = true
dir = "/data/filerldb"

# Alternative: MySQL for production
# [mysql]
# enabled = false
# hostname = "mysql"
# port = 3306
# database = "seaweedfs"
# username = "root"
# <password field omitted - set via SOPS>
# connection_max_idle = 2
# connection_max_open = 100

# Security settings
[security]
# JWT signing key (generated on init)
# signing_key = ""

# Read/write permissions
# read = "admin:admin"
# write = "admin:admin"

# Filer options
[filer]
# Maximum file size for direct upload
max_file_size = 0  # 0 = unlimited

# WebDAV support
[webdav]
enabled = true
```

**S3 Gateway Configuration**:

```json
// config/s3.json
{
  "identities": [
    {
      "name": "admin",
      "credentials": [
        {
          "accessKey": "admin",
          "secretKey": "admin"
        }
      ],
      "actions": [
        "Read",
        "Write",
        "List",
        "Delete",
        "Admin"
      ]
    },
    {
      "name": "readonly",
      "credentials": [
        {
          "accessKey": "readonly",
          "secretKey": "readonly"
        }
      ],
      "actions": [
        "Read",
        "List"
      ]
    }
  ]
}
```

**Certificate Generation** (from existing Ansible patterns):

```bash
#!/bin/bash
# scripts/generate-certs.sh

CERT_DIR="./certs"
CA_CERT="${CERT_DIR}/SeaweedFS_CA.crt"
CA_KEY="${CERT_DIR}/SeaweedFS_CA.key"

mkdir -p "$CERT_DIR"

# Generate CA if not exists
if [ ! -f "$CA_CERT" ]; then
    openssl genrsa -out "$CA_KEY" 2048
    openssl req -new -x509 -days 3650 \
        -key "$CA_KEY" \
        -out "$CA_CERT" \
        -subj "/CN=SeaweedFS CA"
fi

# Generate certificates for each component
for component in master-1 master-2 master-3 volume filer s3; do
    KEY="${CERT_DIR}/${component}.key"
    CSR="${CERT_DIR}/${component}.csr"
    CRT="${CERT_DIR}/${component}.crt"

    openssl genrsa -out "$KEY" 2048
    openssl req -new -key "$KEY" -out "$CSR" \
        -subj "/CN=${component}"
    openssl x509 -req -days 3650 -in "$CSR" \
        -CA "$CA_CERT" -CAkey "$CA_KEY" -CAcreateserial \
        -out "$CRT"

    rm "$CSR"
done

echo "Certificates generated in $CERT_DIR"
```

**MCP Stack Containerfile** (from tinyland.dev patterns):

```dockerfile
# Containerfile.mcp
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    curl \
    jq \
    podman \
    git \
    vim \
    && rm -rf /var/lib/apt/lists/*

# Install Chapel
RUN curl -L https://github.com/chapel-lang/chapel/releases/download/1.33.0/chapel-1.33.0.tar.gz | \
    tar -xz -C /opt && \
    mv /opt/chapel-1.33.0 /opt/chapel

ENV PATH="/opt/chapel/bin:${PATH}"
ENV CHPL_HOME=/opt/chapel

# Create workspace
WORKDIR /workspace

# Setup entrypoint
COPY scripts/mcp-entrypoint.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/mcp-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/mcp-entrypoint.sh"]
```

### GitLab Application Registration

**GitLab OAuth Application Settings**:

1. Navigate to: GitLab → User Settings → Applications
2. Create new application:
   - Name: `TummyCrypt`
   - Redirect URI: `http://localhost:8080/oauth/callback`
   - Scopes: `api`, `read_api`, `read_user`
3. Note:
   - Application ID → `GITLAB_CLIENT_ID`
   - Application Secret → `GITLAB_CLIENT_SECRET`

**GitLab Device Flow Configuration**:

```bash
# .env.local
GITLAB_INSTANCE="https://gitlab.example.com"
GITLAB_CLIENT_ID="your-app-id"
GITLAB_CLIENT_SECRET="your-app-secret"
GITLAB_REDIRECT_URI="http://localhost:8080/oauth/callback"
```

### Development Tools Installation

```bash
#!/bin/bash
# scripts/setup-dev.sh

# Go 1.23+
GO_VERSION="1.23.0"
wget "https://go.dev/dl/go${GO_VERSION}.linux-amd64.tar.gz"
sudo tar -C /usr/local -xzf "go${GO_VERSION}.linux-amd64.tar.gz"
export PATH="/usr/local/go/bin:${PATH}"

# Chapel 1.33
wget https://github.com/chapel-lang/chapel/releases/download/1.33.0/chapel-1.33.0.tar.gz
tar -xz -C /opt chapel-1.33.0.tar.gz
export CHPL_HOME=/opt/chapel-1.33.0
export PATH="${CHPL_HOME}/bin:${PATH}"

# Podman
sudo apt-get update
sudo apt-get install -y podman podman-compose

# Protocol Buffers
wget https://github.com/protocolbuffers/protobuf/releases/download/v25.0/protoc-25.0-linux-x86_64.zip
unzip protoc-25.0-linux-x86_64.zip -d /usr/local

# Chapel QuickCheck
git clone https://github.com/example/chapelCheck ~/git/chapelCheck
cd ~/git/chapelCheck
make install

echo "Development environment setup complete!"
```

---

## Build & Compilation

### Go Build System (Taskfile.yaml)

```yaml
# Taskfile.yaml
version: '3'

vars:
  BINARY_NAME: tummycrypt
  BINARY_PATH: ./bin/{{.BINARY_NAME}}
  VERSION:
    sh: git describe --tags --always --dirty 2>/dev/null || echo "dev"
  LDFLAGS:
    -ldflags="-s -w -X main.Version={{.VERSION}}"

tasks:
  default:
    cmds:
      - task: build

  build:
    desc: Build Go binary
    dir: .
    sources:
      - go.mod
      - go.sum
      - cmd/**/*.go
      - internal/**/*.go
    generates:
      - ./{{.BINARY_PATH}}
    cmds:
      - go build {{.LDFLAGS}} -o {{.BINARY_PATH}} ./cmd

  build-chapel:
    desc: Build Chapel compute engine
    dir: ./chapel
    sources:
      - Main.chpl
      - Config.chpl
      - Diff.chpl
      - Checksum.chpl
      - Chunk.chpl
      - Bridge.chpl
    generates:
      - ./bin/chapel_engine
    cmds:
      - chpl --fast -o ../bin/chapel_engine Main.chpl

  build-all:
    desc: Build all components
    deps:
      - build
      - build-chapel

  build-protos:
    desc: Generate Protocol Buffer code
    dir: ./ipc
    sources:
      - tummycrypt.proto
    generates:
      - tummycrypt.pb.go
    cmds:
      - protoc --go_out=. --go_opt=paths=source_relative tummycrypt.proto

  clean:
    desc: Clean build artifacts
    cmds:
      - rm -rf ./bin
      - rm -f ./ipc/*.pb.go

  test:
    desc: Run all tests
    cmds:
      - task: test-go
      - task: test-chapel

  test-go:
    desc: Run Go tests
    cmds:
      - go test -v -race -coverprofile=coverage.out ./...

  test-chapel:
    desc: Run Chapel tests
    dir: ./chapel/tests
    cmds:
      - chpl TestDiff.chpl -M .. -o test-diff
      - ./test-diff
      - chpl TestChecksum.chpl -M .. -o test-checksum
      - ./test-checksum

  dev:
    desc: Run in development mode with hot reload
    cmds:
      - air
    sources:
      - cmd/**/*.go
      - internal/**/*.go

  docker-build:
    desc: Build Docker images
    cmds:
      - docker build -f Containerfile.daemon -t tummycrypt:latest .

  docker-run:
    desc: Run Docker container
    deps:
      - docker-build
    cmds:
      - docker run -it --rm tummycrypt:latest
```

### Chapel Build System (Makefile)

```makefile
# chapel/Makefile
CHAPEL_HOME ?= /opt/chapel-1.33.0
CHPL = $(CHAPEL_HOME)/bin/chpl
CHPL_FLAGS = --fast --no-checks
CHPL_FLAGS += -I/usr/include -L/usr/lib
CHPL_FLAGS += -M ~/git/chapelCheck/src  # QuickCheck library

# Source files
SOURCES = Main.chpl Config.chpl Logger.chpl \
          Diff.chpl Checksum.chpl Chunk.chpl Bridge.chpl

# Build targets
TARGET = ../bin/chapel_engine

.PHONY: all build clean test test-unit test-integration

all: build

build: $(TARGET)

$(TARGET): $(SOURCES)
	$(CHPL) $(CHPL_FLAGS) -o $@ Main.chpl

clean:
	rm -f $(TARGET) tests/test-*

test: test-unit test-integration

test-unit:
	$(CHPL) tests/TestDiff.chpl $(CHPL_FLAGS) -M ~/git/chapelCheck/src -o tests/test-diff
	./tests/test-diff

	$(CHPL) tests/TestChecksum.chpl $(CHPL_FLAGS) -M ~/git/chapelCheck/src -o tests/test-checksum
	./tests/test-checksum

test-integration:
	$(CHPL) tests/TestBridge.chpl $(CHPL_FLAGS) -M ~/git/chapelCheck/src -o tests/test-bridge
	./tests/test-bridge

# Debug build
debug: CHPL_FLAGS = -g --checks
debug: clean build

# Static build for containers
static: CHPL_FLAGS += --static
static: clean build
```

### GoReleaser Configuration

```yaml
# .goreleaser.yml
project_name: tummycrypt

builds:
  - id: daemon
    main: ./cmd
    binary: tummycryptd
    goos:
      - linux
      - darwin
      - windows
    goarch:
      - amd64
      - arm64
    env:
      - CGO_ENABLED=1
    ldflags:
      - -s -w
      - -X main.Version={{.Version}}
      - -X main.Commit={{.Commit}}
      - -X main.Date={{.Date}}

  - id: chapel-engine
    main: ./chapel/Main.chpl
    binary: chapel_engine
    skip: true  # Built separately with Chapel compiler

universal_binaries:
  - replace: true

archives:
  - format: tar.gz
    name_template: "{{ .ProjectName }}-{{ .Version }}-{{ .Os }}-{{ .Arch }}"
    files:
      - LICENSE
      - README.md

checksum:
  name_template: "{{ .ProjectName }}-{{ .Version }}-checksums.txt"

snapshot:
  name_template: "{{ .Tag }}-next"

changelog:
  sort: asc
  filters:
    exclude:
      - '^docs:'
      - '^test:'
```

---

## Testing Strategy

### Go Testing Patterns (from Crush)

**Unit Tests**:

```go
// internal/sync/delta_test.go
package sync

import (
    "testing"
    "github.com/stretchr/testify/assert"
    "github.com/stretchr/testify/require"
)

func TestDelta_Compute(t *testing.T) {
    tests := []struct {
        name     string
        local    []FileMetadata
        remote   []FileMetadata
        expected Delta
    }{
        {
            name:   "no changes",
            local:  []FileMetadata{{Path: "file.txt", Checksum: "abc123"}},
            remote: []FileMetadata{{Path: "file.txt", Checksum: "abc123"}},
            expected: Delta{
                Added:    nil,
                Modified: nil,
                Deleted:  nil,
            },
        },
        {
            name:   "new file added",
            local:  []FileMetadata{{Path: "new.txt", Checksum: "def456"}},
            remote: []FileMetadata{},
            expected: Delta{
                Added:    []FileMetadata{{Path: "new.txt", Checksum: "def456"}},
                Modified: nil,
                Deleted:  nil,
            },
        },
    }

    for _, tt := range tests {
        t.Run(tt.name, func(t *testing.T) {
            delta := ComputeDelta(tt.local, tt.remote)
            assert.Equal(t, tt.expected.Added, delta.Added)
            assert.Equal(t, tt.expected.Modified, delta.Modified)
            assert.Equal(t, tt.expected.Deleted, delta.Deleted)
        })
    }
}
```

**Golden File Testing**:

```go
// internal/sync/diff_golden_test.go
package sync

import (
    "os"
    "path/filepath"
    "testing"
)

func TestDiff_Golden(t *testing.T) {
    goldenFiles, err := filepath.Glob("testdata/*.golden")
    require.NoError(t, err)

    for _, goldenFile := range goldenFiles {
        t.Run(filepath.Base(goldenFile), func(t *testing.T) {
            // Load test data
            testData := goldenFile[:len(goldenFile)-7] + ".json"
            local, remote := loadTestData(testData)

            // Compute diff
            diff := ComputeDiff(local, remote)

            // Compare with golden file
            expected, err := os.ReadFile(goldenFile)
            require.NoError(t, err)

            actual, err := json.MarshalIndent(diff, "", "  ")
            require.NoError(t, err)

            assert.JSONEq(t, string(expected), string(actual))

            // Update golden file if flag is set
            if os.Getenv("UPDATE_GOLDEN") == "true" {
                os.WriteFile(goldenFile, actual, 0644)
            }
        })
    }
}
```

**Integration Tests with Build Tags**:

```go
// +build integration

// internal/sync/integration_test.go
package sync

import (
    "testing"
    "time"
)

func TestIntegration_SyncToSeaweedFS(t *testing.T) {
    if testing.Short() {
        t.Skip("Skipping integration test in short mode")
    }

    // Start local SeaweedFS
    master := startSeaweedFSMaster(t)
    defer master.Stop()

    filer := startSeaweedFSFiler(t, master)
    defer filer.Stop()

    // Create test data
    testData := createTestFiles(t, "/tmp/tummy-test")

    // Run sync
    engine := NewEngine("/tmp/tummy-test", filer.Endpoint())
    result, err := engine.Run(context.Background())
    require.NoError(t, err)
    assert.Equal(t, 10, result.Transferred)

    // Verify files in SeaweedFS
    files, err := filer.ListFiles("/")
    require.NoError(t, err)
    assert.Len(t, files, 10)
}
```

### Chapel Property-Based Testing (QuickCheck)

```chapel
// chapel/tests/TestDiff.chpl
use Diff;
use chapelCheck;
use Patterns;

// Property: Diff is idempotent
var idempotentProp = property(
    "computing diff twice yields same result",
    fileTreeGen(),
    proc(tree: FileTree) {
        var diff1 = computeDiff(tree);
        var diff2 = computeDiff(tree);
        return diff1 == diff2;
    }
);

// Property: Symmetric diff
var symmetricProp = property(
    "diff(A, B) is inverse of diff(B, A)",
    tupleGen(fileTreeGen(), fileTreeGen()),
    proc((a, b): (FileTree, FileTree)) {
        var diffAB = computeDiff(a, b);
        var diffBA = computeDiff(b, a);

        // Added in AB should be deleted in BA
        if diffAB.added.size != diffBA.deleted.size {
            return false;
        }

        // Deleted in AB should be added in BA
        if diffAB.deleted.size != diffBA.added.size {
            return false;
        }

        return true;
    }
);

// Property: Round-trip
var roundTripProp = property(
    "apply(diff(A, B)) to A yields B",
    tupleGen(fileTreeGen(), fileTreeGen()),
    proc((a, b): (FileTree, FileTree)) {
        var diff = computeDiff(a, b);
        var result = applyDiff(a, diff);
        return result == b;
    }
);

// Custom generator for file trees
proc fileTreeGen() {
    return map(
        listGen(stringGen(1, 10), 0, 20),
        proc(files: list(string)): FileTree {
            var tree: FileTree;
            for f in files {
                var meta = new FileMetadata(
                    path = f,
                    size = abs(rand():int) % 1024 * 1024,  // 0-1MB
                    checksum = randomChecksum()
                );
                tree.files.pushBack(meta);
            }
            return tree;
        }
    );
}

proc randomChecksum(): string {
    var chars = "0123456789abcdef";
    var checksum = "";
    for i in 1..64 {
        checksum += chars[abs(rand():int) % chars.size];
    }
    return checksum;
}

proc main() {
    writeln("=== Diff Property-Based Tests ===\n");

    var results: list(TestResult);
    results.pushBack(check(idempotentProp));
    results.pushBack(check(symmetricProp));
    results.pushBack(check(roundTripProp));

    var passed = 0;
    var failed = 0;
    for r in results {
        printResult(r.passed, r.name, r.numTests, r.failureInfo, r.shrunkInfo);
        if r.passed then passed += 1 else failed += 1;
    }

    writeln();
    printSummary(passed, failed, results.size, 0.0);

    if failed > 0 then halt(1);
}
```

**Checksum Property Tests**:

```chapel
// chapel/tests/TestChecksum.chpl
use Checksum;
use chapelCheck;
use Patterns;

// Property: SHA-256 is deterministic
var deterministicProp = property(
    "checksum of same data is always identical",
    stringGen(0, 4096),
    proc(data: string) {
        var checksum1 = computeChecksum(data);
        var checksum2 = computeChecksum(data);
        return checksum1 == checksum2;
    }
);

// Property: Checksum collision resistance
var collisionResistantProp = property(
    "different data produces different checksums",
    tupleGen(stringGen(1, 1024), stringGen(1, 1024)),
    proc((data1, data2): (string, string)) {
        if data1 == data2 then return true;  // Skip same data

        var checksum1 = computeChecksum(data1);
        var checksum2 = computeChecksum(data2);

        return checksum1 != checksum2;
    }
);

// Property: Checksum is commutative for concatenated data
var commutativeProp = property(
    "checksum(A+B) != checksum(A) and checksum(A+B) != checksum(B)",
    tupleGen(stringGen(0, 512), stringGen(0, 512)),
    proc((data1, data2): (string, string)) {
        var checksumAB = computeChecksum(data1 + data2);
        var checksumA = computeChecksum(data1);
        var checksumB = computeChecksum(data2);

        // Concatenated checksum should differ from parts
        if checksumAB == checksumA || checksumAB == checksumB {
            return false;
        }

        return true;
    }
);

// Property: Round-trip
var roundTripProp = property(
    "recomputing checksum yields same result",
    stringGen(0, 8192),
    proc(data: string) {
        var checksum1 = computeChecksum(data);
        var checksum2 = computeChecksum(data);
        return isRoundTrip(data, checksum1, checksum2);
    }
);

proc main() {
    writeln("=== Checksum Property-Based Tests ===\n");

    var results: list(TestResult);
    results.pushBack(check(deterministicProp));
    results.pushBack(check(collisionResistantProp));
    results.pushBack(check(commutativeProp));
    results.pushBack(check(roundTripProp));

    var passed = 0;
    var failed = 0;
    for r in results {
        printResult(r.passed, r.name, r.numTests, r.failureInfo, r.shrunkInfo);
        if r.passed then passed += 1 else failed += 1;
    }

    writeln();
    printSummary(passed, failed, results.size, 0.0);

    if failed > 0 then halt(1);
}
```

### Test Organization

```
tests/
├── unit/
│   ├── internal/
│   │   ├── sync/
│   │   │   ├── delta_test.go
│   │   │   └── conflict_test.go
│   │   ├── seaweedfs/
│   │   │   └── client_test.go
│   │   └── crypto/
│   │       └── encrypt_test.go
│   └── golden/
│       └── sync/
│           └── diff_testdata/
│               ├── scenario1.json
│               ├── scenario1.json.golden
│               ├── scenario2.json
│               └── scenario2.json.golden
├── integration/
│   ├── sync_seaweedfs_test.go
│   └── auth_gitlab_test.go
├── e2e/
│   └── full_sync_workflow_test.go
└── chapel/
    └── tests/
        ├── TestDiff.chpl
        ├── TestChecksum.chpl
        └── TestChunk.chpl
```

---

## Security Architecture

### Credential Management (Crush Patterns)

**Provider Interface**:

```go
// internal/secrets/provider/provider.go
package provider

type Provider interface {
    Get(ctx context.Context, path string) (*Credential, error)
    Set(ctx context.Context, cred *Credential) error
    Delete(ctx context.Context, path string) error
    List(ctx context.Context, prefix string) ([]*Credential, error)
    Exists(ctx context.Context, path string) (bool, error)
    Lock() error
    IsLocked() bool
    Status() ProviderStatus
}

type Unlockable interface {
    Provider
    Unlock(ctx context.Context, passphrase string) error
    UnlockWithOAuth(ctx context.Context, token string) error
}

type Syncable interface {
    Provider
    Sync(ctx context.Context) error
    Push(ctx context.Context) error
    Pull(ctx context.Context) error
}
```

**KDBX Implementation**:

```go
// internal/secrets/keystore/kdbx.go
package keystore

import (
    "context"
    "github.com/tobischo/gokeepasslib/v3"
)

type KDBXProvider struct {
    file       *gokeepasslib.Database
    filePath   string
    passphrase string
    locked     bool
    entryCache map[string]*gokeepasslib.Entry
}

func (p *KDBXProvider) Create(passphrase string) error {
    p.file = gokeepasslib.NewDatabase()
    p.file.Content.Meta.DatabaseName = "TummyCrypt Keystore"
    p.file.Credentials = gokeepasslib.NewPasswordCredentials(passphrase)

    // Initialize hierarchy
    root, _ := p.file.Content.Root.Groups[0].NewEntry("SeaweedFS", "")
    root.Value.Set("AccessKey", "")

    return p.file.Unlock(p.file.Credentials)
}

func (p *KDBXProvider) Unlock(ctx context.Context, passphrase string) error {
    p.passphrase = passphrase
    p.locked = false

    // Build entry cache for fast lookups
    p.entryCache = make(map[string]*gokeepasslib.Entry)
    p.buildCache(p.file.Content.Root.Groups[0])

    return nil
}

func (p *KDBXProvider) Get(ctx context.Context, path string) (*Credential, error) {
    if p.locked {
        return nil, ErrKeystoreLocked
    }

    entry, ok := p.entryCache[path]
    if !ok {
        return nil, ErrCredentialNotFound
    }

    return &Credential{
        Path:  path,
        Value: entry.GetContent(),
    }, nil
}

func (p *KDBXProvider) Set(ctx context.Context, cred *Credential) error {
    if p.locked {
        return ErrKeystoreLocked
    }

    entry := gokeepasslib.NewEntry(
        filepath.Base(cred.Path),
        filepath.Dir(cred.Path),
    )
    entry.Value.Set("Value", cred.Value)

    p.entryCache[cred.Path] = entry
    return nil
}
```

**TPM Device Binding**:

```go
// internal/secrets/binding/tpm.go
package binding

import (
    "crypto/sha256"
    "encoding/hex"
    "github.com/golang/crypto/tpm2"
)

type TPMBinding struct {
    ekHandle tpm2.Handle
    aikCert  []byte
}

func NewTPMBinding() (*TPMBinding, error) {
    // Open TPM device
    rwc, err := tpm2.OpenTPM()
    if err != nil {
        return nil, err
    }
    defer rwc.Close()

    // Create Endorsement Key (EK)
    ekHandle, pub, err := tpm2.CreatePrimary(rwc, tpm2.TPMRHEndorsement, tpm2.AlgRSA)
    if err != nil {
        return nil, err
    }

    // Create Attestation Identity Key (AIK)
    aikHandle, aikPub, err := tpm2.CreatePrimary(rwc, tpm2.TPMRHOwner, tpm2.AlgRSA)
    if err != nil {
        return nil, err
    }

    // Certify AIK with EK
    cert, err := tpm2.Certify(rwc, aikHandle, ekHandle, nil)
    if err != nil {
        return nil, err
    }

    return &TPMBinding{
        ekHandle: ekHandle,
        aikCert:  cert,
    }, nil
}

func (t *TPMBinding) DeriveBindingKey() []byte {
    // Derive binding key from AIK certificate
    hash := sha256.Sum256(t.aikCert)
    return hash[:]
}

func (t *TPMBinding) Attest(challenge []byte) ([]byte, error) {
    // Sign challenge with AIK
    rwc, err := tpm2.OpenTPM()
    if err != nil {
        return nil, err
    }
    defer rwc.Close()

    signature, err := tpm2.Sign(rwc, t.aikHandle, challenge, nil)
    if err != nil {
        return nil, err
    }

    return signature, nil
}
```

### GitLab Device Enrollment

```go
// internal/secrets/gitlab/enrollment.go
package gitlab

import (
    "context"
    "encoding/json"
    "fmt"
)

type DeviceEnrollment struct {
    Client *OAuthClient
    TPM    *TPMBinding
}

type DeviceInfo struct {
    DeviceName    string `json:"device_name"`
    MachineID     string `json:"machine_id"`
    TPMCert       string `json:"tpm_cert"`
    DeviceType    string `json:"device_type"`
    OSVersion     string `json:"os_version"`
    Architecture  string `json:"architecture"`
}

func (e *DeviceEnrollment) Enroll(ctx context.Context) error {
    // 1. Start OAuth device flow
    authResp, err := e.Client.StartDeviceFlow(ctx)
    if err != nil {
        return fmt.Errorf("failed to start device flow: %w", err)
    }

    // 2. Display user code
    fmt.Printf("Visit: %s\n", authResp.VerificationURI)
    fmt.Printf("Enter code: %s\n", authResp.UserCode)

    // 3. Poll for token
    token, err := e.Client.PollForToken(ctx, authResp.DeviceCode,
        time.Duration(authResp.Interval)*time.Second)
    if err != nil {
        return fmt.Errorf("failed to get token: %w", err)
    }

    // 4. Derive machine binding
    machineID := hex.EncodeToString(e.TPM.DeriveBindingKey())

    // 5. Create device enrollment
    device := DeviceInfo{
        DeviceName:   getHostname(),
        MachineID:    machineID,
        TPMCert:      hex.EncodeToString(e.TPM.aikCert),
        DeviceType:   "laptop",
        OSVersion:    getOSVersion(),
        Architecture: runtime.GOARCH,
    }

    // 6. Send to GitLab
    reqBody, _ := json.Marshal(device)
    req, _ := http.NewRequest("POST",
        fmt.Sprintf("%s/api/v4/user/devices", e.Client.authBaseURL),
        bytes.NewReader(reqBody))
    req.Header.Set("Authorization", "Bearer "+token.AccessToken)
    req.Header.Set("Content-Type", "application/json")

    resp, err := http.DefaultClient.Do(req)
    if err != nil {
        return fmt.Errorf("failed to enroll device: %w", err)
    }
    defer resp.Body.Close()

    if resp.StatusCode != http.StatusCreated {
        return fmt.Errorf("device enrollment failed: %s", resp.Status)
    }

    return nil
}
```

### Encryption Strategy

```go
// internal/crypto/encrypt.go
package crypto

import (
    "crypto/aes"
    "crypto/cipher"
    "crypto/rand"
    "io"
)

const (
    KeySize   = 32  // AES-256
    NonceSize = 12  // GCM standard
)

func GenerateKey() ([]byte, error) {
    key := make([]byte, KeySize)
    if _, err := rand.Read(key); err != nil {
        return nil, err
    }
    return key, nil
}

func Encrypt(plaintext []byte, key []byte) ([]byte, error) {
    block, err := aes.NewCipher(key)
    if err != nil {
        return nil, err
    }

    gcm, err := cipher.NewGCM(block)
    if err != nil {
        return nil, err
    }

    nonce := make([]byte, NonceSize)
    if _, err := rand.Read(nonce); err != nil {
        return nil, err
    }

    ciphertext := gcm.Seal(nil, nonce, plaintext, nil)

    // Prepend nonce for decryption
    return append(nonce, ciphertext...), nil
}

func Decrypt(ciphertext []byte, key []byte) ([]byte, error) {
    if len(ciphertext) < NonceSize {
        return nil, fmt.Errorf("ciphertext too short")
    }

    block, err := aes.NewCipher(key)
    if err != nil {
        return nil, err
    }

    gcm, err := cipher.NewGCM(block)
    if err != nil {
        return nil, err
    }

    nonce := ciphertext[:NonceSize]
    ciphertext = ciphertext[NonceSize:]

    plaintext, err := gcm.Open(nil, nonce, ciphertext, nil)
    if err != nil {
        return nil, err
    }

    return plaintext, nil
}
```

---

## Infrastructure Requirements

### Minimum System Requirements

**Development Machine**:
- CPU: 4+ cores (for Chapel parallelism)
- RAM: 8GB minimum, 16GB recommended
- Storage: 50GB available space
- OS: Linux (Ubuntu 22.04+), macOS 12+, Windows 11

**SeaweedFS Cluster (Production)**:
- Master Nodes: 3x (2 vCPU, 4GB RAM, 50GB SSD each)
- Volume Nodes: 3+ (4 vCPU, 8GB RAM, 500GB SSD each)
- Filer Nodes: 2+ (2 vCPU, 4GB RAM, 100GB SSD each)
- Network: 10Gbps internal, 1Gbps external

### GitLab Requirements

**GitLab Instance**:
- Version: GitLab CE/EE 16.0+
- OAuth2: Enabled
- Device Flow: Supported
- User scope: `api`, `read_api`, `read_user`
- Application: "TummyCrypt" registered

**GitLab Configuration**:

```nginx
# GitLab nginx configuration for OAuth callback
location /oauth/callback {
    proxy_pass http://gitlab:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

### Container Runtime

**Podman Configuration**:

```bash
# /etc/containers/registries.conf
[registries.search]
registries = ['docker.io', 'quay.io', 'registry.gitlab.com']

[registries.insecure]
registries = []

[registries.block]
registries = []
```

**Podman Network**:

```bash
# Create dedicated network
podman network create seaweed-net \
    --subnet=10.0.0.0/24 \
    --gateway=10.0.0.1 \
    --dns=8.8.8.8
```

**PVC Configuration**:

```bash
# Create persistent volumes
podman volume create seaweed-master-1
podman volume create seaweed-master-2
podman volume create seaweed-master-3
podman volume create seaweed-volume
podman volume create seaweed-filer
podman volume create tummycrypt-mcp
```

### Certificate Management

**Certificate Authority**:
- CA Certificate: `certs/SeaweedFS_CA.crt`
- CA Key: `certs/SeaweedFS_CA.key` (keep secret!)

**Component Certificates**:
- Master nodes: `certs/master-{1,2,3}.{key,crt}`
- Volume server: `certs/volume.{key,crt}`
- Filer: `certs/filer.{key,crt}`
- S3 Gateway: `certs/s3.{key,crt}`

**Certificate Rotation Procedure**:

```bash
#!/bin/bash
# scripts/rotate-certs.sh

CERT_DIR="./certs"
BACKUP_DIR="${CERT_DIR}/backup/$(date +%Y%m%d_%H%M%S)"

mkdir -p "$BACKUP_DIR"

# Backup existing certificates
cp -r ${CERT_DIR}/*.key ${CERT_DIR}/*.crt "$BACKUP_DIR/"

# Regenerate certificates
./scripts/generate-certs.sh

# Update containers
podman-compose restart seaweed-master-1 seaweed-master-2 seaweed-master-3
podman-compose restart seaweed-volume seaweed-filer seaweed-s3

echo "Certificates rotated. Backup at: $BACKUP_DIR"
```

---

## Deployment Patterns

### Systemd Service Files

**Linux (daemon)**:

```ini
# /etc/systemd/system/tummycryptd.service
[Unit]
Description=TummyCrypt Secure Sync Daemon
After=network.target

[Service]
Type=simple
User=tummycrypt
Group=tummycrypt
ExecStart=/usr/local/bin/tummycryptd daemon \
    --config /etc/tummycrypt/config.yaml \
    --data-dir /var/lib/tummycrypt
Restart=on-failure
RestartSec=10

# Security
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/tummycrypt

[Install]
WantedBy=multi-user.target
```

**macOS (daemon)**:

```xml
<!-- /Library/LaunchDaemons/com.tummycrypt.daemon.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.tummycrypt.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/tummycryptd</string>
        <string>daemon</string>
        <string>--config</string>
        <string>/etc/tummycrypt/config.yaml</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/var/log/tummycryptd.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/tummycryptd.err</string>
</dict>
</plist>
```

**Windows (daemon)**:

```xml
<!-- C:\Program Files\TummyCrypt\tummycryptd.xml -->
<service>
  <id>tummycryptd</id>
  <name>TummyCrypt Daemon</name>
  <description>Secure synchronization daemon for SeaweedFS</description>
  <executable>C:\Program Files\TummyCrypt\tummycryptd.exe</executable>
  <arguments>daemon --config "C:\ProgramData\TummyCrypt\config.yaml"</arguments>
  <logpath>C:\ProgramData\TummyCrypt\logs</logpath>
  <log mode="roll-by-size">
    <sizeThreshold>10240</sizeThreshold>
    <keepFiles>8</keepFiles>
  </log>
  <onfailure action="restart" delay="10 sec"/>
</service>
```

### Configuration Files

**Main Config**:

```yaml
# /etc/tummycrypt/config.yaml
version: "1.0"

server:
  host: "0.0.0.0"
  port: 8080
  metrics_port: 9090

seaweedfs:
  filer_endpoint: "http://seaweed-filer:8888"
  s3_endpoint: "http://seaweed-s3:8333"
  access_key: "<from-sops>"
  # secret credential key also loaded from SOPS
  tls:
    enabled: true
    ca_cert: "/etc/tummycrypt/certs/SeaweedFS_CA.crt"

chapel:
  enabled: true
  engine_path: "/usr/local/bin/chapel_engine"
  parallelism: 4
  max_chunk_size: 65536  # 64KB

encryption:
  algorithm: "AES-256-GCM"
  key_derivation: "Argon2id"
  iterations: 3
  memory: 64MiB
  parallelism: 4

sync:
  default_conflict: "newer"
  default_delete: "never"
  chunk_size: 5242880  # 5MB
  max_retries: 3
  retry_delay: 5s

authentication:
  provider: "kdbx"
  keystore_path: "/etc/tummycrypt/keystore.kdbx"
  tpm_binding: true
  gitlab_oauth:
    instance: "https://gitlab.example.com"
    client_id: "${GITLAB_CLIENT_ID}"
    client_secret: "${GITLAB_CLIENT_SECRET}"

logging:
  level: "info"
  format: "json"
  output:
    - type: "file"
      path: "/var/log/tummycrypt/tummycryptd.log"
      max_size: 100MiB
      max_age: 30d
      max_backups: 10
```

### Docker Deployment

**Daemon Containerfile**:

```dockerfile
# Containerfile.daemon
FROM golang:1.23-alpine AS builder

# Install dependencies
RUN apk add --no-cache \
    git \
    make \
    gcc \
    musl-dev

# Build Go binary
WORKDIR /build
COPY go.mod go.sum ./
RUN go mod download

COPY . .
RUN CGO_ENABLED=1 GOOS=linux go build \
    -ldflags="-s -w -X main.Version={{VERSION}}" \
    -o tummycryptd ./cmd

# Runtime image
FROM alpine:3.19

RUN apk add --no-cache \
    ca-certificates \
    libgcc

COPY --from=builder /build/tummycryptd /usr/local/bin/

# Create user
RUN adduser -D -u 1000 -h /var/lib/tummycrypt tummycrypt

# Setup directories
RUN mkdir -p /etc/tummycrypt /var/lib/tummycrypt /var/log/tummycrypt
RUN chown -R tummycrypt:tummycrypt /etc/tummycrypt /var/lib/tummycrypt /var/log/tummycrypt

USER tummycrypt

# Health check
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD wget --spider -q http://localhost:8080/health || exit 1

EXPOSE 8080 9090

ENTRYPOINT ["/usr/local/bin/tummycryptd"]
CMD ["daemon"]
```

**Chapel Engine Containerfile**:

```dockerfile
# Containerfile.chapel
FROM chapel/chapel:1.33.0 AS builder

WORKDIR /build

# Copy Chapel source
COPY chapel/ ./chapel/
COPY Chapelfile .

# Build static binary
RUN chpl --fast --static --no-checks -o chapel_engine chapel/Main.chpl

# Runtime image
FROM alpine:3.19

RUN apk add --no-cache ca-certificates

COPY --from=builder /build/chapel_engine /usr/local/bin/

RUN chmod +x /usr/local/bin/chapel_engine

USER nobody:nobody

ENTRYPOINT ["/usr/local/bin/chapel_engine"]
```

---

## Code Patterns & Best Practices

### Go Patterns (from Crush)

**Error Handling**:

```go
// Use fmt.Errorf with %w for error wrapping
func (e *Engine) sync(ctx context.Context) error {
    diff, err := e.computeDiff(ctx)
    if err != nil {
        return fmt.Errorf("failed to compute diff: %w", err)
    }

    if err := e.transferFiles(ctx, diff); err != nil {
        return fmt.Errorf("failed to transfer files: %w", err)
    }

    return nil
}

// Custom error types
var (
    ErrKeystoreLocked    = errors.New("keystore is locked")
    ErrCredentialNotFound = errors.New("credential not found")
    ErrInvalidChecksum    = errors.New("invalid checksum")
    ErrSyncConflict      = errors.New("sync conflict")
)
```

**Context Usage**:

```go
// Always accept and propagate context
func (e *Engine) sync(ctx context.Context) error {
    // Create child context with timeout
    ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
    defer cancel()

    // Check for cancellation
    select {
    case <-ctx.Done():
        return ctx.Err()
    default:
        // Continue processing
    }
}
```

**Logging**:

```go
import "log/slog"

// Structured logging
logger := slog.Default()

logger.Info("Starting sync",
    "local_path", e.localPath,
    "remote_path", e.remotePath,
    "files", len(diff.Files),
)

logger.Error("Transfer failed",
    "file", file.Path,
    "error", err.Error(),
)
```

### Chapel Patterns (from blahaj/mail-api)

**Parallel Loops**:

```chapel
// Use forall for parallel iteration
forall file in files {
    var checksum = computeChecksum(file);
    results[file] = checksum;
}

// Use coforall for fire-and-forget
coforall tid in 1..numWorkers {
    processQueue(tid);
}

// Use atomic for thread-safe operations
var counter: atomic int;

forall file in files {
    counter.add(1);
    // ... process file ...
}
```

**Records vs Classes**:

```chapel
// Use records for value types (preferred)
record FileMetadata {
    var path: string;
    var size: int;
    var checksum: string;
}

// Use classes for reference types (when needed)
class FileHandle {
    var fd: c_ptr(FILE);

    proc ref close() {
        fclose(fd);
    }
}
```

**FFI Patterns**:

```chapel
require "openssl/hmac.h";
require "-lssl", "-lcrypto";

extern proc HMAC(
    evp_md: c_ptr(void),
    key: c_ptrConst(c_uchar),
    key_len: c_int,
    data: c_ptrConst(c_uchar),
    data_len: c_size_t,
    md: c_ptr(c_uchar),
    md_len: c_ptr(c_uint)
): c_ptr(c_uchar);

extern proc EVP_sha256(): c_ptr(void);

proc hmacSHA256(data: string, key: string): string {
    var result: [0..31] uint(8);
    var resultLen: c_uint = 32;

    HMAC(
        EVP_sha256(),
        key.c_str(): c_ptrConst(c_uchar),
        key.numBytes: c_int,
        data.c_str(): c_ptrConst(c_uchar),
        data.numBytes: c_size_t,
        c_ptrTo(result[0]),
        c_ptrTo(resultLen)
    );

    return bytesToHex(result, resultLen: int);
}
```

### Container Patterns (from tinyland.dev)

**Multi-Stage Builds**:

```dockerfile
# Stage 1: Dependencies
FROM golang:1.23-alpine AS deps
WORKDIR /app
COPY go.mod go.sum ./
RUN go mod download

# Stage 2: Builder
FROM golang:1.23-alpine AS builder
WORKDIR /app
COPY --from=deps /app/go.mod go.sum ./
COPY . .
RUN CGO_ENABLED=1 go build -o tummycryptd ./cmd

# Stage 3: Runtime
FROM alpine:3.19
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/tummycryptd /usr/local/bin/
USER nobody
ENTRYPOINT ["/usr/local/bin/tummycryptd"]
```

**Health Checks**:

```yaml
# docker-compose.yml
healthcheck:
  test: ["CMD", "wget", "--spider", "-q", "http://localhost:8080/health"]
  interval: 30s
  timeout: 5s
  retries: 3
  start_period: 10s
```

**Volume Mounts**:

```yaml
volumes:
  - ./data:/data:z            # :z for SELinux
  - /var/run/podman/podman.sock:/var/run/podman/podman.sock:ro
```

---

## Development Cycle

### Prerequisites

1. **Install Tools**:
   ```bash
   ./scripts/setup-dev.sh
   ```

2. **Start Local Stack**:
   ```bash
   podman-compose up -d
   podman-compose logs -f seaweed-filer
   ```

3. **Generate Certificates**:
   ```bash
   ./scripts/generate-certs.sh
   ```

4. **Register GitLab App**:
   - Create OAuth application in GitLab
   - Update `.env.local` with credentials

### Workflow

**Feature Development**:

```bash
# 1. Create feature branch
git checkout -b feature/sync-optimization

# 2. Implement feature
vim internal/sync/engine.go

# 3. Write tests
vim internal/sync/engine_test.go

# 4. Run tests
task test

# 5. Build all components
task build-all

# 6. Run in dev mode
task dev

# 7. Test manually
tummycrypt sync ~/test-data tummycrypt:/test

# 8. Commit changes
git add .
git commit -m "Optimize sync algorithm with parallel chunking"

# 9. Push to remote
git push origin feature/sync-optimization
```

**Debugging**:

```bash
# Enable debug logging
tummycrypt --debug sync ~/data tummycrypt:/backup

# Attach Go debugger
dlv debug ./cmd -- --config config.yaml

# Chapel debug build
cd chapel
make debug
./chapel_engine --verbose

# View logs
journalctl -u tummycryptd -f

# Container logs
podman logs -f seaweed-filer
```

**Testing**:

```bash
# Run all tests
task test

# Run only Go tests
task test-go

# Run only Chapel tests
task test-chapel

# Run with coverage
go test -v -race -coverprofile=coverage.out ./...

# Update golden files
UPDATE_GOLDEN=true go test ./tests/golden

# Property-based tests
cd chapel/tests
./test-diff --numTests=10000
```

**Performance Profiling**:

```bash
# CPU profile
tummycrypt sync ~/data tummycrypt:/backup --profile=cpu.prof

# Memory profile
tummycrypt sync ~/data tummycrypt:/backup --profile=mem.prof

# Analyze profiles
go tool pprof cpu.prof
go tool pprof mem.prof

# Chapel profiling
export CHPL_RT_NUM_THREADS_PER_LOCALE=4
export dataParMaxChunkSize=64
./chapel_engine --profile
```

**Code Review Checklist**:

- [ ] Tests pass (`task test`)
- [ ] Lint passes (`golangci-lint run`)
- [ ] Documentation updated
- [ ] Error handling complete
- [ ] Logging added where needed
- [ ] Security review passed
- [ ] Performance benchmarks acceptable
- [ ] Chapel parallelism appropriate
- [ ] FFI bounds checking disabled in release

**Release Process**:

```bash
# 1. Bump version
vim VERSION
git commit -am "Bump version to 1.2.3"

# 2. Tag release
git tag -a v1.2.3 -m "Release v1.2.3"

# 3. Build release binaries
goreleaser release --clean

# 4. Verify binaries
./dist/tummycrypt_1.2.3_linux_amd64/tummycryptd --version

# 5. Push tags
git push origin main --tags
```

### Troubleshooting

**SeaweedFS Not Starting**:

```bash
# Check logs
podman logs seaweed-master-1
podman logs seaweed-filer

# Verify network
podman network inspect seaweed-net

# Check certificates
openssl x509 -in certs/master-1.crt -text -noout
```

**Chapel Build Failures**:

```bash
# Check Chapel version
chpl --version

# Verify library paths
echo $CHPL_HOME

# Clean build
make clean
make debug
```

**GitLab OAuth Failures**:

```bash
# Check credentials
cat .env.local

# Test OAuth flow manually
tummycrypt auth --verbose

# Verify GitLab instance
curl https://gitlab.example.com/oauth/authorize_device
```

**Sync Issues**:

```bash
# Check daemon status
systemctl status tummycryptd

# View detailed logs
journalctl -u tummycryptd -n 100

# Check SeaweedFS connectivity
curl http://seaweed-filer:8888/

# Test credentials
tummycrypt status --check-credentials
```

---

## Appendix

### A. Environment Variables

```bash
# TummyCrypt Configuration
TUMMYCRYPT_CONFIG=/etc/tummycrypt/config.yaml
TUMMYCRYPT_DATA_DIR=/var/lib/tummycrypt
TUMMYCRYPT_LOG_LEVEL=info
TUMMYCRYPT_DEBUG=false

# SeaweedFS Connection
SEAWEEDFS_FILER_ENDPOINT=http://seaweed-filer:8888
SEAWEEDFS_S3_ENDPOINT=http://seaweed-s3:8333
SEAWEEDFS_ACCESS_KEY=<from-sops>
# SEAWEEDFS secret key also set here (value from SOPS)

# GitLab OAuth
GITLAB_INSTANCE=https://gitlab.example.com
GITLAB_CLIENT_ID=your-client-id
GITLAB_CLIENT_SECRET=your-client-secret
GITLAB_REDIRECT_URI=http://localhost:8080/oauth/callback

# Chapel Configuration
CHPL_RT_NUM_THREADS_PER_LOCALE=4
dataParMaxChunkSize=65536

# Encryption
TUMMYCRYPT_KEY_ITERATIONS=3
TUMMYCRYPT_KEY_MEMORY=64
TUMMYCRYPT_KEY_PARALLELISM=4
```

### B. File System Layout

```
/etc/tummycrypt/
├── config.yaml
├── keystore.kdbx
└── certs/
    ├── SeaweedFS_CA.crt
    ├── SeaweedFS_CA.key
    ├── master-1.{key,crt}
    ├── master-2.{key,crt}
    ├── master-3.{key,crt}
    ├── volume.{key,crt}
    ├── filer.{key,crt}
    └── s3.{key,crt}

/var/lib/tummycrypt/
├── data/
├── cache/
└── sessions/

/var/log/tummycrypt/
├── tummycryptd.log
├── tummycryptd.err
└── sync.log

~/.local/share/tummycrypt/
└── .session.json
```

### C. Port Mapping

| Service | Port | Protocol | Description |
|---------|------|----------|-------------|
| tummycryptd | 8080 | HTTP | Daemon API |
| tummycryptd | 9090 | HTTP | Prometheus metrics |
| seaweed-master-* | 9333 | HTTP | Master API |
| seaweed-master-* | 19333 | gRPC | Master gRPC |
| seaweed-volume | 8080 | HTTP | Volume API |
| seaweed-volume | 18080 | gRPC | Volume gRPC |
| seaweed-filer | 8888 | HTTP | Filer API |
| seaweed-filer | 18888 | gRPC | Filer gRPC |
| seaweed-s3 | 8333 | HTTP | S3 Gateway |

### D. Resource Limits

**Development**:

```yaml
resources:
  limits:
    memory: 512Mi
    cpu: "1"
  requests:
    memory: 256Mi
    cpu: "0.5"
```

**Production**:

```yaml
resources:
  limits:
    memory: 4Gi
    cpu: "4"
  requests:
    memory: 2Gi
    cpu: "2"
```

---

## Changelog

### Version 1.0.0 (2025-01-01)

**Initial Design Document**
- Comprehensive architecture specification
- Go/Chapel hybrid implementation plan
- KDBX + GitLab OAuth authentication
- SeaweedFS native API integration
- Chapel QuickCheck property-based testing
- Podman compose local development stack
- Systemd/macOS LaunchD/Windows service files
- Complete build system with Taskfile.yaml and Makefile
- Testing strategy with unit, integration, and E2E tests
- Security architecture with TPM device binding
- Deployment patterns for Docker and bare metal

**Next Steps**:
1. Create project structure
2. Implement Go daemon framework
3. Implement Chapel compute engine stubs
4. Set up local SeaweedFS development stack
5. Implement KDBX provider
6. Implement GitLab OAuth device flow
7. Create basic sync algorithm
8. Write comprehensive tests
9. Add CLI with Cobra + Bubbletea
10. Deploy to local podman stack for testing

---

**Document Status**: ✅ Design Complete - Ready for Implementation

**Next Review**: After prototype implementation (Phase 1)

**Maintainer**: TummyCrypt Development Team

**Contributors**: See git log for contributors
