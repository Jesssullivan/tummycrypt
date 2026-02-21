# Research Findings: TummyCrypt Retooling

## Executive Summary

This document compiles research findings for the TummyCrypt retooling initiative, which aims to transform an experimental Ansible-based SeaweedFS deployment into a state-of-the-art, secure S3 synchronization and backup system.

## Research Goals

1. Secure S3-driven directory synchronization
2. Chapel Language parallel file operations and performance
3. TPM-based device enrollment and attestation
4. KeePassXC + GitLab SecureEnclave integration patterns
5. SeaweedFS filer API native usage and optimization

## 1. Secure S3 Directory Synchronization

### Key Findings

#### Encryption Best Practices (Industry Standard)

**At Rest:**
- **Server-Side Encryption (SSE)** is baseline and automatic for all S3 objects
  - SSE-S3: Amazon-managed keys (simplest, no control)
  - SSE-KMS: AWS Key Management Service keys (auditability, granular access control)
  - SSE-C: Customer-provided keys (full control, client-side management)
  - **Best Practice**: Use SSE-KMS for high-security environments requiring auditability

**In Transit:**
- HTTPS/TLS is mandatory for all data transfers
- Enforce HTTPS-only bucket policies: `aws:SecureTransport = "false"`
- Do not pin S3 certificates - AWS rotates them automatically

#### S3 Block Public Access
- Default setting for all new buckets prevents accidental exposure
- Can be overridden by specific IAM or bucket policies
- **Critical**: Enable at account AND bucket level

#### Versioning and Data Protection
- **S3 Versioning**: Protects against accidental/malicious deletions
  - Allows recovery of previous versions
  - All operations create new versions by default
  - **Trade-off**: Additional storage cost for versioning overhead
- **S3 Object Lock**: WORM (Write-Once-Read-Many) protection
  - Prevents object modification or deletion for specified period
  - Use case: Compliance requirements (e.g., financial records)

#### Access Control Strategies
- **IAM Policies**: User/role-based permissions vs bucket policies
  - IAM: Attach permissions to identities/roles (user-based)
  - Bucket Policies: Resource-level access control
  - **Principle of Least Privilege**: Grant minimum necessary access only
- **Conditional Access**: Use IP restrictions, time-based conditions, MFA requirements

#### Advanced Security Patterns

**Client-Side Encryption:**
- Encrypt data before uploading to S3
- Manage encryption keys securely (separate from storage)
- Use strong algorithms: AES-256-GCM with authenticated encryption (AEAD)
- Rotate keys independently of S3 server keys

**Certificate-Based Authentication:**
- Mutual TLS (mTLS) for service-to-service communication
- Certificate pinning for critical services
- Regular certificate rotation and expiration policies

#### File Synchronization Algorithms

**Comparison Algorithms:**
- **Byte-by-byte**: Simple but no detailed diff information
- **Line-by-line**: Better for text files, more context
- **Chunk/Block-based**: Efficient for large files, allows parallel comparison
- **Tree-based**: Best for structured data (XML, JSON)
- **Hybrid approaches**: Combine multiple methods based on file type

**Checksums for Integrity:**
- SHA-256: Default, secure, widely supported
- MD5: Faster but has known collision risks
- Verify checksums before/after transfers to detect corruption

#### Conflict Resolution Strategies

**Common Strategies:**

1. **Last Write Wins (LWW)**: Simple, timestamp-based
   - Pros: Easy to implement, deterministic
   - Cons: Can lose data, depends on synchronized clocks

2. **Version Control**: Maintain history of all changes
   - Pros: No data loss, can restore any version
   - Cons: Storage overhead, complexity

3. **Manual Resolution**: Prompt user on conflicts
   - Pros: User makes informed decision
   - Cons: Interrupts automation

4. **CRDT (Conflict-Free Replicated Data Types)**: Merge without coordination
   - Pros: Automatic, mathematical correctness guarantees
   - Cons: Limited to specific data types (counters, sets)
   - Best for: Counters, sets, registers

5. **Semantic Conflict Detection**: Use content-aware merging
   - Detect based on file type (code vs binary)
   - Use file-specific merge strategies (e.g., 3-way merge for code)

#### Delta Transfer and Optimization

**Minimize Data Transfer:**
- Only transfer changed chunks (differences)
- Use compression for upload/download
- Parallel chunk upload for large files
- Chunk size optimization: 5-50MiB commonly recommended
- Deduplication: Avoid transferring identical objects

**Network Optimization:**
- Use multipart upload for files >5MiB
- Enable transfer acceleration (where available)
- Optimize for high-latency networks: larger chunks, fewer operations

## 2. Chapel Language Parallel File Operations

### Key Capabilities

#### Parallel Programming Model
- **Locales**: Multi-node and multi-core support
- **Task Parallelism**: High-level abstraction for concurrent tasks
- **Data Parallelism**: Operations on distributed arrays/structures
- **Nested Parallelism**: Support for combining parallelism types
- **Global View**: Unified view of distributed data

#### Performance Characteristics

**Strengths:**
- **Productivity**: Clear, concise syntax similar to Python
- **Scalability**: Scales from laptops to supercomputers
- **Locality Control**: Explicit placement of data/computations
- **Portability**: Runs on diverse architectures (Linux, macOS, HPC)

**Compiler Optimizations:**
- LLVM backend for native code generation
- SIMD vectorization for compute-bound workloads
- Distributed memory optimizations
- Aggressive inlining and loop unrolling with `--fast`

#### File I/O Patterns

**Parallel I/O Module:**
```chapel
use IO;

// Read file in parallel
forall i in 0..numFiles-1 with (ref, fileName) {
  var f = open(fileName, iomode.r);
  // Process file content...
}
```

**Massive Scale File Processing:**
```chapel
config const numTasks = here.maxTasksPerLocale;
var inputFiles: [] string;

forall fileName in inputFiles with (ref, idx) {
  var file = openReader(fileName, iomode.r);
  var chunkSize = calculateOptimalChunkSize(file.size);
  // Process chunk in parallel across locales
}
```

**Performance Features:**
- Memory bandwidth optimization: `dataParMaxChunkSize` configuration
- Tunable parallelism: Adjust via environment variables
- Limit resource usage: `--numTasksPerLocale` flag
- Fine-grained synchronization: Built-in sync primitives

#### Integration Patterns

**Go Interoperability:**
- Chapel can be compiled to C library for FFI calls
- Shared memory regions for Go/Chapel communication
- Message passing via channels for coordination
- Use Go for system-level services, Chapel for compute-intensive tasks

### Resource Limiting and Control

**Environment Variables:**
- `CHPL_RT_NUM_THREADS_PER_LOCALE`: Threads per locale
- `CHPL_TARGET_COMPILER`: LLVM, GNU, or Cray
- `CHPL_MEM_STRATEGY`: Memory allocation strategy
- `CHPL_LOCAL_BLOCK_SIZE`: Tunable block size

**Built-in Profiling:**
- `--profile` flag for detailed performance metrics
- `--timeTasks` for timing analysis
- `--memTrack` for memory usage tracking

**Runtime Control:**
- `--fast`: Disable bounds checking (production builds)
- `--no-local` or `--only-local` for locality control
- `--setLocaleMask` and `--setTaskMask` for selective execution

#### Filesystem Integration

**Direct File Access:**
```chapel
use Path;
use FileSystem;

// Native filesystem operations
var dir = opendir("/data");
forall entry in dir {
  // Process files in parallel
}
```

**S3-Compatible API Patterns:**
- Implement chunked upload/download matching S3 multipart
- Support resume capability for interrupted transfers
- Implement parallel directory listing and comparison
- Add metadata handling (ETags, custom headers)

## 3. TPM-Based Device Enrollment

### Microsoft Windows Enrollment Attestation

#### Attestation Components

**Device Identity:**
- **EK (Endorsement Key)**: Unique per TPM, signed by manufacturer
- **AIK (Attestation Identity Key)**: Per-device, signed by enrollment CA
- **IdevID (Initial Device ID)**: Certificate authority for device identity
- **CertChain**: EK ← AIK ← Root CA

**Attestation Flow:**
1. Device generates EK (TPM creates)
2. EK certified by manufacturer
3. Server signs AIK certificate
4. Device binds AIK to TPM
5. TPM creates attestation quote proving AIK-EK binding
6. Verifier checks cert chain and signature

#### Security Properties

**Key Protection:**
- Keys never leave TPM in plaintext
- EK cannot be duplicated without TPM reset
- AIK can be rotated, EK persists for device lifetime
- Secure key storage in TPM NVRAM

**Attestation Evidence:**
- PCR (Platform Configuration Register) values included in quote
- Nonces prove software/boot state
- Countermeasures against replay attacks
- Cryptographic binding to specific device

#### Enrollment Workflow

**Provisioning:**
- `tpm2_takeownership`: Initialize TPM
- `tpm2_createprimary`: Create signing key
- `tpm2_createek`: Create EK
- `tpm2_activatecredential`: Activate EK
- Certificate requests to CA for signing

**Platform Integration:**
- Windows Intune: MDM integration and device attestation reports
- Linux: `tpm2-tools` package for TPM operations
- OpenSSL/CryptoAPI: For key generation and signing

#### Implementation Considerations

**Certificate Management:**
- Use X.509 certificates with proper CA hierarchy
- Implement certificate rotation policies
- Store certificates in TPM NVRAM or secure storage
- Support certificate revocation checking

**Device Binding:**
- Bind certificates to TPM EK for device identity
- Use measured boot values in attestation
- Implement anti-rollback protections

## 4. KeePassXC + GitLab SecureEnclave Integration

### Credential Storage Pattern

**KeePassXC Database Structure:**
- Entry: Title, Username, URL, Notes, Password
- Groups: Organize related entries
- Custom Fields: Device enrollment status, TPM attestation certs
- Tags: Category management (e.g., "Production", "Personal", "Work")

**Secrets Management:**
- Master password for database unlock
- Entry passwords: AES-256 encrypted (with key derivation)
- Secure key derivation: Use KeePassXC's built-in KDF
- Auto-type: Database, TOTP, Hardware key (YubiKey)

### GitLab SecureEnclave Integration

**Authentication Flow:**
- OAuth 2.0 / OIDC flow for GitLab SSO
- User redirects to GitLab for authentication
- Application receives authorization code
- Exchange code for access tokens

**Device Enrollment Pattern:**
```go
type EnrollmentState struct {
    DeviceID      string
    TPMAttestation string
    EnrollmentDate  time.Time
}

func EnrollDevice() error {
    // 1. Trigger GitLab OAuth
    // 2. Get device TPM EK
    // 3. Perform Windows attestation
    // 4. Store enrollment state in KeePassXC
}
```

**Secure Storage:**
- Store GitLab tokens in KeePassXC with TOTP
- Encrypt device certificates with entry passwords
- Use custom fields for device metadata

**CLI Pattern:**
```bash
tummycrypt device enroll --interactive
# Prompts for GitLab credentials
# Performs TPM attestation
# Stores results in KeePassXC
# Returns enrollment status
```

## 5. SeaweedFS Filer API and Native Usage

### SeaweedFS Architecture

**Core Components:**
- **Masters (3x)**: Metadata management, leader election
- **Volume Servers**: Data storage with replication
- **Filers**: S3-compatible API, metadata indexing
- **Clients**: rclone/crypt mount points

**Filer-Specific Features:**
- S3 API compatibility (PUT, GET, DELETE, LIST, COPY)
- WebDAV support for legacy protocols
- Metadata operations (stat, chmod, chown)
- Directory operations (mkdir, rmdir, rename)
- FUSE mount support for local access

### Native API Usage Patterns

**Optimization Strategies:**

**1. Use Filer Direct API**
```go
// Direct Filer gRPC connection
import "github.com/seaweedfs/seaweedfs-client-go"

client := filer.NewClient("filer01:8888", clientOption{
    // Direct connection to SeaweedFS filer
    GrpcDialOption: 500ms timeout,
    FilerClient: &client, // Direct client
})

// S3-compatible operations
files, err := client.ListDirectory("/", filer.ListOptions{
    StartFileName: "",
    Limit:           1000,
})

upload, err := client.Upload("localfile", "/remote/path", filer.UploadOption{
    MaxMB:         100,  // Multipart upload
    ChunkSize:       5*1024*1024,  // 5MB chunks
    DisableChunking:  true,  // Better for large files
})
```

**2. Leverage WebDAV for Direct File Operations**
```chapel
// Mount SeaweedFS via WebDAV
use WebDAV;
var client = new WebDAVClient("http://filer01:8888/webdav");
var files = client.listFiles("/");
forall file in files with (ref, fileInfo) {
    // Native filesystem-like operations
    var content = client.download(fileInfo.path);
    // Process with Chapel parallelism
}
```

**3. Native Filer Socket API**
```go
// Use filer socket for bulk operations
import "github.com/seaweedfs/seaweedfs-client-go/pkg/filer/socket"

socketClient := filer.NewSocketClient("filer01:8888", filer.SocketOption{
    // High-performance socket operations
    LocalHost:        "192.168.1.171", // Drobo volume
    RetryOnNetworkError: true,
})
```

**4. Efficient Directory Traversal**
```chapel
// Parallel directory listing and metadata extraction
use Path;
var dirPath = "/data";
var metadata = listDir(dirPath, followLinks=false);

// Metadata operations in parallel
forall entry in metadata {
    // Extract size, modtime, etc.
    var fileMD5 = calculateChecksum(entry.path);
}
```

**5. Batch Processing for Throughput**
```chapel
// Process multiple files in parallel
config const batchSize = 1000;
var files = listDir("/backup", recursive=true);
for (fileBatch, idx) in 0.. by batchSize {
    coforall file in fileBatch with (ref, batchIdx) {
        // Parallel upload/download
    }
}
```

### Performance Optimization Patterns

**Connection Pooling:**
```go
// Reuse filer connections
type FilerPool struct {
    clients []*FilerClient
    mutex    sync.Mutex
}

func (p *FilerPool) Get() *FilerClient {
    p.mutex.Lock()
    defer p.mutex.Unlock()
    if len(p.clients) == 0 {
        client, _ := filer.NewClient(...)
        p.clients = append(p.clients, client)
    }
    return p.clients[rand.Intn(len(p.clients))]
}
```

**Chunked Transfer Tuning:**
```go
// Adaptive chunk sizing based on network conditions
func calculateChunkSize(fileSize int64, bandwidth float64) int64 {
    const optimalChunk = int64(5 * 1024 * 1024) // 5MB
    if bandwidth < 10*1024*1024 {
        return optimalChunk / 2  // Smaller on slow networks
    } else if bandwidth > 100*1024*1024 {
        return optimalChunk * 2  // Larger on fast networks
    }
    return optimalChunk
}
```

**Metadata Caching:**
```go
// Cache SeaweedFS directory listings and file metadata
type MetadataCache struct {
    entries map[string]*DirectoryEntry
    ttl      time.Duration
    mu       sync.RWMutex
}

func (c *MetadataCache) Get(path string) (*DirectoryEntry, error) {
    c.mu.RLock()
    defer c.mu.RUnlock()
    entry, exists := c.entries[path]
    if !exists || time.Since(entry.timestamp) > c.ttl {
        // Fetch from filer
        dirEntries, err := c.client.ListDirectory(path, ...)
        if err == nil {
            c.entries[path] = &dirEntries
            return &dirEntries[0], nil
        }
    }
    return entry, nil
}
```

**Conflict Resolution with Filer Metadata**
```go
// Use SeaweedFS's versioning for conflict-free sync
type SyncState struct {
    LocalPath    string
    RemotePath   string
    LocalMD5    string
    RemoteMD5   string
    LocalSize   int64
    RemoteSize  int64
    Conflict     ConflictType
}

func (s *Sync) ResolveConflict(state SyncState) error {
    switch state.Conflict {
    case ConflictNone:
        // No action needed
    case ConflictNewerLocal:
        // Upload local version
    case ConflictNewerRemote:
        // Download remote version
    case ConflictContentDifference:
        // Merge content (for simple files)
        // Or create local version with timestamp suffix
    }
}
```

## 6. Design Phase Preparation

### Proposed Architecture

#### High-Level Components

**1. System Daemon (Go)**
- Configuration management and persistence
- File system watching and event queuing
- Network communication with SeaweedFS filers
- TPM attestation and key management
- S3-compatible API adapter layer
- CLI command processor and user interaction

**2. Compute Engine (Chapel)**
- Parallel file diffing and comparison algorithms
- Parallel checksum calculation for integrity verification
- Chunk-based file processing for large file operations
- Background worker pool for processing tasks
- Resource-aware task scheduling

**3. Security Layer (Go + TPM)**
- KeePassXC database integration for credential storage
- TPM attestation flow implementation
- Certificate management and validation
- GitLab OAuth integration for SSO
- Secure enclave operations for sensitive data

**4. User Interface (Go CLI)**
- Interactive enrollment and configuration commands
- Directory watch and sync status display
- Device management and attestation status
- Certificate and credential management UI
- Progress and error reporting

#### Technology Stack Selection

**Go Libraries:**
- `seaweedfs/seaweedfs-client-go`: Official SeaweedFS Go client
- `github.com/seaweedfs/seaweedfs-client-go/pkg/filer/socket`: Socket API for bulk operations
- `github.com/elastic/go-elasticsearch`: For metadata indexing (optional)
- `keepassxc/v2`: KeePassXC database access
- `golang.org/x/crypto`: Cryptographic operations
- `github.com/golang/crypto/tpm2`: TPM operations
- `github.com/zitadel/trust`: Certificate management
- `gopkg.in/oauth2`: OAuth2/OIDC for GitLab

**Chapel Components:**
- Standard Chapel distribution for portability
- Custom domain mapping for SeaweedFS directory structures
- FFI layer for calling Go functions from Chapel
- I/O modules for parallel file operations

**Development Tools:**
- Go 1.23+ for system daemon
- Chapel 1.33+ for compute engine
- Protocol Buffers for Go/Chapel IPC
- Protocol definition files (protobuf, flatbuffers)
- Systemd/service files for daemon deployment

### Implementation Phases

**Phase 1: Core Infrastructure (Weeks 1-2)**
- Go daemon framework and CLI scaffolding
- Configuration file format and validation
- Basic SeaweedFS filer client integration
- TPM attestation basic implementation

**Phase 2: Security Integration (Weeks 2-3)**
- KeePassXC database operations
- TPM attestation workflow
- Certificate management system
- GitLab OAuth integration
- Secure credential storage and retrieval

**Phase 3: Compute Engine (Weeks 3-4)**
- Chapel parallel file operations implementation
- Go-Chapel IPC protocol design
- File diffing algorithms in Chapel
- Checksum calculation and verification
- Chunk processing and batch operations

**Phase 4: Optimization and Testing (Weeks 5-6)**
- Performance profiling and optimization
- Resource limiting and tuning
- Conflict resolution implementation
- Integration testing with existing SeaweedFS infrastructure
- Load testing and stress testing

**Phase 5: Production Deployment (Weeks 7-8)**
- Systemd service files for Linux
- macOS launchd service files for macOS
- Windows service files for Windows
- Migration from Ansible to new daemon
- Documentation and user guides

### Key Design Decisions

**Parallelism Strategy:**
- Use Chapel for CPU-bound, data-parallel tasks (file comparison, checksums)
- Use Go for I/O-bound, network-bound tasks (SeaweedFS API calls)
- Limit Chapel worker pool to prevent resource exhaustion
- Use message passing for Go/Chapel coordination

**Security Architecture:**
- Defense in depth: Multiple layers (encryption, TPM attestation, certificate validation)
- Zero-trust architecture: Minimize trust assumptions
- Secure credential storage: KeePassXC with master password + entry encryption
- Device binding: TPM binding of certificates to hardware
- Secure enclave patterns: Isolate sensitive operations

**Performance vs. Security Trade-offs:**
- End-to-end encryption adds computational overhead
- Parallel checksums improve integrity detection
- Chunked transfers reduce network impact on large files
- TPM attestation adds enrollment latency (one-time cost)
- Caching reduces SeaweedFS API calls
- Use adaptive algorithms based on network conditions

## Next Steps

1. **Share with team** examples of Go code patterns, Chapel algorithms, and tool integrations used in `../crush/` project
2. **Design detailed architecture** including data flow diagrams and component interactions
3. **Create technical specification** for Go-Chapel IPC protocol
4. **Define API contracts** for daemon components and CLI commands
5. **Implement prototype** for critical path (e.g., Chapel file diffing)
6. **Plan migration strategy** from existing Ansible deployment to new daemon
