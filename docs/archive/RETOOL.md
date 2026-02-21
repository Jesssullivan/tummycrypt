# RETOOL.md

## Project Retooling: TummyCrypt → Secure S3 Synchronization System

### Vision

Transform TummyCrypt from an experimental Ansible-based SeaweedFS deployment into a state-of-the-art, secure S3-driven directory synchronization and backup system built with Go and Chapel Language.

### Architecture Goals

1. **Security-First Design**
   - Adopt identical security paradigm to `../crush/` solution:
     - KeePassXC (.kdbx) for credential storage
     - GitLab SecureEnclave client for SSO/identity
     - TPM-friendly device enrollment and attestation
     - Certificate-based authentication throughout

2. **SeaweedFS-Native Integration**
   - Leverage SeaweedFS filer structures natively
   - Work directly with SeaweedFS API and directory hierarchy
   - Maintain existing infrastructure investment

3. **Hybrid Language Stack**
   - **Go**: Application daemon for file queuing, diffing, syncing
     - Handles network I/O, S3 operations, system integration
     - Provides system daemon capabilities
   - **Chapel Language**: High-performance core operations
     - Highly parallel file processing
     - Tunable and limitable resource usage
     - Scalable development model
     - Advanced synchronization algorithms

4. **System Daemon + CLI**
   - Background system daemon for continuous synchronization
   - Friendly CLI interface for:
     - Device enrollment (TPM-based)
     - Directory enrollment and watching
     - Certificate management
     - SeaweedFS path configuration
     - Credential settings
     - SSO via GitLab

### Core Components to Research

1. **Secure S3 Directory Synchronization**
   - Latest approaches to secure S3 syncing
   - Diff algorithms for large-scale directory trees
   - Conflict resolution strategies
   - Encryption-at-rest and in-transit patterns
   - Optimized delta transfer mechanisms

2. **Chapel Language Capabilities**
   - File I/O and parallel processing patterns
   - Integration with Go via FFI or inter-process communication
   - Resource limiting and tuning mechanisms
   - Best practices for Chapel-based daemon components

3. **Security Architecture Patterns**
   - TPM 2.0 device enrollment flows
   - KeePassXC database integration patterns
   - GitLab SSO/OAuth2 integration
   - Certificate management and rotation
   - Secure credential storage and retrieval

4. **SeaweedFS Advanced Features**
   - Filer API usage patterns
   - Efficient directory traversal and metadata operations
   - S3 compatibility layer optimization
   - Multi-master synchronization considerations

5. **Modern S3 Synchronization Research**
   - Industry best practices (e.g., rclone, AWS CLI, MinIO Client)
   - Academic research on distributed file synchronization
   - Performance optimization techniques
   - Security hardening approaches

### Phases

1. **Research Phase** (Current)
   - Deep investigation via Perplexity, Web Prime, arXiv, and MCP tools
   - Focus on secure S3 sync, Chapel capabilities, security paradigms
   - Compile findings for architecture design

2. **Design Phase** (Next)
   - Architecture specification for Go + Chapel codebase
   - Work phase breakdown
   - Development stack selection
   - Integration patterns between components

3. **Implementation Phase**
   - Core daemon development
   - CLI development
   - Security integration
   - SeaweedFS native operations

### Success Criteria

- Industry-leading security posture (matching crush/ paradigm)
- Highly scalable and performant synchronization
- Native SeaweedFS integration (not just S3 compatibility)
- User-friendly enrollment and configuration
- System-level daemon reliability
- Chapel Language demonstrating clear performance benefits


- Preserve certificate and credential management approach
