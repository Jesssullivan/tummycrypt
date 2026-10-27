-- The estate's blob topology as of 2026-10-27, one row per component.
-- Status is the live reading where one was taken (BL-2 estate-reality,
-- kubectl about 19:42Z and 20:10Z), otherwise the ruling. Versions are the
-- deployed ones; upstream latest is in `source` when it differs.
-- Long-form source list: Linear document "BL-3: blob topology layers and
-- TCFS interfaces (2026-10-27)".
let T = ./Types.dhall

let Check = ./Check.dhall

let L = T.Layer

let P = T.Product

let R = T.Role

let St = T.Status

let C = T.Concern

let CP = T.CommitPoint

let row =
      λ(name : Text) →
      λ(layer : T.Layer) →
      λ(product : T.Product) →
      λ(role : T.Role) →
      λ(status : T.Status) →
      λ(version : Optional Text) →
      λ(concern : T.Concern) →
      λ(commitPoint : Optional T.CommitPoint) →
      λ(source : Text) →
          { name
          , layer
          , product
          , role
          , status
          , version
          , concern
          , commitPoint
          , source
          }
        : T.Component

let none = None Text

let noCommit = None T.CommitPoint

let components =
      [ row
          "neo Glyph ATOM RAID0 USB SSD (4 TB, 3 APFS containers since 2026-10-26)"
          L.Media
          P.Lab
          R.Leverage
          St.Live
          none
          C.DiskMedia
          noCommit
          "lab docs/agent-notes/2026-10-25-sess-neo-glyph-rca-recovery-runbook.md (lost writes on a RAID0 USB bridge)"
      , row
          "sting NVMe P310 (fast-local), M550 (cache), 860 DCT (data)"
          L.Media
          P.Lab
          R.Leverage
          St.Live
          none
          C.DiskMedia
          noCommit
          "BL-2 estate-reality section 2"
      , row
          "honey ST4000DM000 HDD /archive (SeaweedFS volume dir)"
          L.Media
          P.Blahaj
          R.Leverage
          St.Live
          none
          C.DiskMedia
          noCommit
          "lab vars/knob_register.json fleet.seaweedfs live"
      , row
          "bumble ZFS pools tank and bumble-ssd"
          L.Media
          P.Blahaj
          R.Leverage
          St.Live
          none
          C.DiskMedia
          noCommit
          "BL-2 estate-reality section 2 (zpools ONLINE)"
      , row
          "XFS with project quotas (sting /srv/fast-local/jess)"
          L.Volume
          P.Lab
          R.Leverage
          St.Live
          none
          C.HostVolumes
          noCommit
          "lab vars/knob_register.json sting.xfs.fast-local.imaxpct"
      , row
          "APFS volumes (neo Glyph, PZM disk5 quota volumes)"
          L.Volume
          P.Apple
          R.Leverage
          St.Live
          none
          C.HostVolumes
          noCommit
          "lab AGENTS.md PZM storage; nix/lib/pzm-disk5-volumes.nix"
      , row
          "softconnect NFS loopback hold for the Darwin Nix store"
          L.Volume
          P.Softconnect
          R.Own
          St.Proposed
          none
          C.NixStoreHold
          noCommit
          "softconnect build/dhall-spec-20261026 spec/dhall/Mechanism.dhall"
      , row
          "Cluster volumes: local-path, OpenEBS ZFS on bumble, OpenEBS LVM"
          L.Volume
          P.Blahaj
          R.Leverage
          St.Live
          none
          C.ClusterVolumes
          noCommit
          "BL-2 estate-reality section 3.1"
      , row
          "Longhorn V2 (0 volumes; Phase 5 pilot ruled)"
          L.Volume
          P.Blahaj
          R.Leverage
          St.Ruled
          (Some "1.12.1")
          C.ClusterVolumes
          noCommit
          "lab vars/knob_register.json fleet.longhorn (R-D15-SATA-20261016)"
      , row
          "SeaweedFS master, volume, filer and S3 gateway (s3 0/0, s3_hold true, replication 000)"
          L.ObjectStore
          P.Upstream
          R.Leverage
          St.Paused
          (Some "4.48 (upstream 4.51, 2026-10-25)")
          C.ObjectStorage
          noCommit
          "blahaj tofu/config/seaweedfs-honey.tfvars.json; Q153; R-C760, R-C777, R-C778"
      , row
          "RustFS (Attic and tofu state only; no TCFS on RustFS)"
          L.ObjectStore
          P.Upstream
          R.Excluded
          St.Live
          (Some "1.0.0-beta.8 (upstream 1.0.4)")
          C.ObjectStorage
          noCommit
          "BL-2 brief section 5; GHSA-566f-q62r-wcr8, GHSA-7gcx-wg4x-q9x6"
      , row
          "Garage (documented fallback, WAN multi-site design)"
          L.ObjectStore
          P.Upstream
          R.Fallback
          St.Absent
          (Some "v2.5.0 upstream")
          C.ObjectStorage
          noCommit
          "blahaj 2026-09-28 storage-backend-convergence-design spec :189"
      , row
          "MinIO CE (archived) and AIStor"
          L.ObjectStore
          P.Vendor
          R.Excluded
          St.Absent
          (Some "RELEASE.2025-10-15T17-29-55Z (last CE tag)")
          C.ObjectStorage
          noCommit
          "github.com/minio/minio README; BL-2 brief section 5"
      , row
          "DreamObjects off-site S3 (CNPG leg live; s3repl leg not seeded)"
          L.ObjectStore
          P.Vendor
          R.Leverage
          St.Ruled
          none
          C.ObjectStorage
          noCommit
          "lab knob register fleet.seaweedfs target (filer.backup to tinyland-bk-s3repl, R-C777)"
      , row
          "Tigris, Cloudflare R2, S3 Express One Zone (edge or off-site candidates)"
          L.ObjectStore
          P.Vendor
          R.Aspirational
          St.Absent
          none
          C.ObjectStorage
          noCommit
          "BL-3 section 1 (vendor consistency pages)"
      , row
          "TCFS index and manifests (path-index publication by conditional write)"
          L.Namespace
          P.Tcfs
          R.Own
          St.Paused
          (Some "0.12.17 deployed; main 185 commits ahead")
          C.FileNamespace
          (Some CP.ObjectConditionalWrite)
          "crates/tcfs-storage/src/operator.rs verify_conditional_write_semantics; crates/tcfs-sync/src/index_entry.rs"
      , row
          "SeaweedFS filer on CNPG Postgres"
          L.Namespace
          P.Upstream
          R.Leverage
          St.Live
          (Some "4.48")
          C.ObjectStorage
          (Some CP.SqlTransaction)
          "BL-2 estate-reality section 3.2"
      , row
          "NATS JetStream streams SYNC_TASKS, HYDRATION_EVENTS, STATE_UPDATES"
          L.Namespace
          P.Tcfs
          R.Leverage
          St.Paused
          (Some "nats 2.10 (old stack, 0/0)")
          C.ChangeFeed
          (Some CP.LogAppend)
          "crates/tcfs-sync/src/nats.rs module docs"
      , row
          "Apache Iceberg, Delta Lake, DuckLake, lakeFS (table and versioned-data catalogs)"
          L.Namespace
          P.Upstream
          R.Sibling
          St.Absent
          (Some "Iceberg 1.12.0, Delta 4.4.1, DuckLake 1.0, lakeFS 1.88.0")
          C.TableCatalog
          noCommit
          "BL-3 section 1; no such workload in the honey cluster (kubectl 2026-10-27)"
      , row
          "Milvus (Woodpecker WAL and Loon manifests on object storage) and pgvector"
          L.Namespace
          P.Upstream
          R.Sibling
          St.Absent
          (Some "Milvus 3.0.4, pgvector 0.8.7")
          C.VectorIndex
          noCommit
          "BL-3 section 1; no Milvus workload in the honey cluster (kubectl 2026-10-27)"
      , row
          "tcfsd sync engine (chunk, encrypt, hydrate, unsync, reconcile)"
          L.SyncEngine
          P.Tcfs
          R.Own
          St.Paused
          (Some "0.12.17")
          C.FileSync
          noCommit
          "BL2-Q10/11: disable tcfsd until the s3_hold flip"
      , row
          "bulkload estate consolidation"
          L.SyncEngine
          P.Bulkload
          R.Interim
          St.Live
          none
          C.EstateConsolidation
          noCommit
          "bulkload docs/SPEC.md; BL2-Q12 publish if it beats rclone"
      , row
          "rclone VFS, JuiceFS CE, weed mount, Mountpoint for S3, ZeroFS"
          L.SyncEngine
          P.Upstream
          R.Baseline
          St.Absent
          (Some "rclone 1.75.2, JuiceFS 1.4.2, Mountpoint 1.24.0, ZeroFS 2.3.6")
          C.FileSync
          noCommit
          "BL2-Q9 No bridge: benchmark baselines only"
      , row
          "tcfs-fuse Linux mount (read-write)"
          L.Surface
          P.Tcfs
          R.Own
          St.Paused
          none
          C.LinuxMount
          noCommit
          "BL-2 repo-tcfs section 2 client layers"
      , row
          "tcfs FileProvider extension (read-only until version-token publication)"
          L.Surface
          P.Tcfs
          R.Own
          St.Live
          (Some "io.tinyland.tcfs.fileprovider 0.2.0")
          C.MacMount
          noCommit
          "crates/tcfs-file-provider/src/direct.rs:21"
      , row
          "tcfs-nfs NFSv3 loopback (read-only fallback)"
          L.Surface
          P.Tcfs
          R.Leverage
          St.Paused
          none
          C.MacMount
          noCommit
          "crates/tcfs-nfs/src/adapter.rs NFS3ERR_ROFS"
      , row
          "Apple FSKit user-space file systems (macOS 15.4+)"
          L.Surface
          P.Apple
          R.Aspirational
          St.Absent
          none
          C.MacMount
          noCommit
          "developer.apple.com/documentation/fskit"
      , row
          "macFUSE kext and FUSE-T"
          L.Surface
          P.Vendor
          R.Excluded
          St.Absent
          (Some "macFUSE 5.4.0, FUSE-T 1.2.9")
          C.MacMount
          noCommit
          "BL-2 sota section 5 (closed kext; commercial licence)"
      , row
          "Windows Cloud Files API provider (skeleton)"
          L.Surface
          P.Tcfs
          R.Aspirational
          St.Proposed
          none
          C.WindowsMount
          noCommit
          "crates/tcfs-cloudfilter (613 lines)"
      , row
          "tcfs CLI and tcfs-mcp control surface"
          L.Surface
          P.Tcfs
          R.Own
          St.Live
          (Some "0.12.17")
          C.AgentControl
          noCommit
          "crates/tcfs-mcp/src/server.rs (7 tools)"
      , row
          "Git repos with linked worktrees, live SQLite, agent sessions"
          L.Workspace
          P.Tcfs
          R.Own
          St.Ruled
          none
          C.WorkspaceState
          noCommit
          "BL2-Q8 TCFS v1 must cover them (TIN-3692 36925e2d)"
      , row
          "bulkload fold and reattach verbs for worktrees (until TCFS v1)"
          L.Workspace
          P.Bulkload
          R.Interim
          St.Ruled
          none
          C.WorkspaceState
          noCommit
          "TIN-7491; BL2-Q2 dated exception, verbs next"
      , row
          "Host path map (/Users/jess/git on neo to /srv/fast-local/jess/git on sting)"
          L.Workspace
          P.Tcfs
          R.Own
          St.Proposed
          none
          C.PathMapping
          noCommit
          "TIN-2301 (Backlog); NC-1 respelled 15 pointer files by hand (TIN-3692 395ab4e0)"
      ]

let _ = assert : Check.layerErrors components ≡ ([] : List Text)

in  components
