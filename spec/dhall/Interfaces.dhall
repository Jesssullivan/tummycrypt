-- The contract between each pair of adjacent layers, as a target. Where
-- the live estate differs, the gap is named in `completeness` with its
-- owner, so the spec states what is unknown instead of hiding it.
let T = ./Types.dhall

let Check = ./Check.dhall

let L = T.Layer

let P = T.Product

let F = T.Failure

let interfaces =
      [ { id = "I-MV media to volume"
        , lower = L.Media
        , upper = L.Volume
        , owner = P.Lab
        , operations =
          [ "block read and write"
          , "flush and FUA write barriers"
          , "discard"
          , "identify by volume UUID, never by device node, label or path"
          ]
        , consistency = T.Consistency.Strong
        , conditional = T.Conditional.NotApplicable
        , identity = T.Identity.VolumeUuid
        , transport = T.Transport.LocalOnly
        , crossesHosts = False
        , onFailure = [ F.Hold, F.ErrorToCaller ]
        , metadata = "filesystem superblock and volume UUID"
        , completeness =
            T.Completeness.Partial
              [ "flush honesty is unmeasured on USB bridges; the neo Glyph RAID0 lost acknowledged writes (lab 2026-10-25 RCA; softconnect FlushHonesty)"
              ]
        }
      , { id = "I-VO volume to object store"
        , lower = L.Volume
        , upper = L.ObjectStore
        , owner = P.Blahaj
        , operations =
          [ "append needles to volume files"
          , "fsync policy per volume server"
          , "vacuum and compaction"
          , "placement by replication string"
          ]
        , consistency = T.Consistency.Strong
        , conditional = T.Conditional.NotApplicable
        , identity = T.Identity.VolumeUuid
        , transport = T.Transport.LocalOnly
        , crossesHosts = False
        , onFailure = [ F.ErrorToCaller, F.Refuse ]
        , metadata = "volume index in memory; volume ids in the master"
        , completeness =
            T.Completeness.Partial
              [ "one volume server on one HDD, replication 000; the bumble rack is ruled (R-C778) and not delivered (blahaj#2219)"
              , "the volume server fsync setting is not recorded in blahaj tofu; unverified"
              ]
        }
      , { id = "I-ON object store to namespace"
        , lower = L.ObjectStore
        , upper = L.Namespace
        , owner = P.Tcfs
        , operations =
          [ "PutObject with If-None-Match: * (create if absent)"
          , "PutObject with If-Match: <etag> (replace if unchanged)"
          , "GetObject with If-Match (exact-version read)"
          , "HeadObject, ListObjectsV2"
          , "DeleteObject on a versioned bucket"
          , "multipart upload for large chunks"
          ]
        , consistency = T.Consistency.Strong
        , conditional = T.Conditional.CreateAndReplace
        , identity = T.Identity.BucketKey
        , transport = T.Transport.Tls
        , crossesHosts = True
        , onFailure =
          [ F.RetryBounded, F.ErrorToCaller, F.AbsenceIsNotDeletion ]
        , metadata =
            "{prefix}/chunks/{hash}, {prefix}/manifests/{file_hash}, {prefix}/index/{rel_path}, tcfs-meta/devices.json"
        , completeness =
            T.Completeness.Partial
              [ "run verify_conditional_write_semantics against the live SeaweedFS endpoint before the s3_hold flip: conditional-write fixes landed after 4.48 (seaweedfs#11780, #11882)"
              , "bucket versioning for orphan GC is unverified on 4.48"
              , "path keys and manifests are plaintext (tummycrypt SPEC open decision on encrypted metadata)"
              , "every fleet config uses plain http endpoints with enforce_tls = false (lab nix/hosts/base.nix:908-913)"
              , "OpenDAL 0.55 has no ETag-conditional delete (crates/tcfs-cli/src/main.rs:2907)"
              ]
        }
      , { id = "I-NS namespace to sync engine"
        , lower = L.Namespace
        , upper = L.SyncEngine
        , owner = P.Tcfs
        , operations =
          [ "read an index entry as Live, Deleted or Missing"
          , "publish Preparing then Committed by compare-and-swap"
          , "write a tombstone by compare-and-swap"
          , "fetch a manifest and its chunks by hash"
          , "subscribe to STATE_UPDATES as a wake-up hint"
          ]
        , consistency = T.Consistency.Strong
        , conditional = T.Conditional.CreateAndReplace
        , identity = T.Identity.DeviceKey
        , transport = T.Transport.Tls
        , crossesHosts = True
        , onFailure = [ F.AbsenceIsNotDeletion, F.KeepBoth, F.RetryBounded ]
        , metadata =
            "index entry {manifest_hash, size, chunks, kind}; device registry signed with a master-derived key"
        , completeness =
            T.Completeness.Partial
              [ "NATS authority is unstated; proposed: the index decides and NATS only wakes readers"
              , "SYNC_TASKS feeds server-side worker pods, which would need content keys under client-side encryption"
              , "wrap_mode = master on every host; per-device wrap is code only (I-45)"
              , "device secret keys are stored raw at mode 600 against device.rs:3-4 (BL2-Q10)"
              ]
        }
      , { id = "I-SS sync engine to surface"
        , lower = L.SyncEngine
        , upper = L.Surface
        , owner = P.Tcfs
        , operations =
          [ "gRPC TcfsDaemon over a Unix socket: Hydrate, Unsync, Pull, PullExact, Push, Watch, ListFiles, ResolveConflict"
          , "FUSE lookup, read, write, fsync, release"
          , "FileProvider enumerate, fetchContents, modifyItem with baseVersion, evict"
          , "NFSv3 read"
          ]
        , consistency = T.Consistency.CloseToOpen
        , conditional = T.Conditional.NotApplicable
        , identity = T.Identity.OsUser
        , transport = T.Transport.LocalOnly
        , crossesHosts = False
        , onFailure = [ F.ErrorToCaller, F.RetryBounded, F.KeepBoth ]
        , metadata = "per-path FileSyncStatus in the local state cache"
        , completeness =
            T.Completeness.Partial
              [ "three hydration paths: CLI pull, FileProvider fetch and the orphaned Hydrate verb (TIN-2683)"
              , "FileProvider and NFS are read-only until version-token conditional publication"
              , "two state vocabularies: tcfs-core FileState (no uses) and tcfs-sync FileSyncStatus; File.dhall proposes one"
              , "the errno and hold policy for an offline read of a stub is unstated"
              ]
        }
      , { id = "I-SW surface to workspace"
        , lower = L.Surface
        , upper = L.Workspace
        , owner = P.Tcfs
        , operations =
          [ "enroll a root"
          , "attach and detach a host session"
          , "request a handoff of the writer lease"
          , "quiesce: no cwd, open file or write in 30 min; SQLite checkpointed; no index.lock; every file Clean or Stub on the source"
          , "respell host paths in gitdir pointers through the host path map"
          , "reconcile diverged writes keep-both"
          ]
        , consistency = T.Consistency.LeaseOrdered
        , conditional = T.Conditional.NotApplicable
        , identity = T.Identity.DeviceKeyAndLease
        , transport = T.Transport.Tls
        , crossesHosts = True
        , onFailure = [ F.Refuse, F.KeepBoth ]
        , metadata =
            "proposed: the lease is an index object written by compare-and-swap, so it adds no server"
        , completeness =
            T.Completeness.Open
              "BL2-Q8 rules that TCFS v1 covers worktrees and live SQLite; no design exists yet. Worktree.dhall is the proposed contract"
        }
      ]

let replication =
      [ { id =
            "R-OFFSITE SeaweedFS filer.backup to DreamObjects tinyland-bk-s3repl"
        , from = "SeaweedFS on honey"
        , to = "DreamObjects (Object Lock)"
        , owner = P.Blahaj
        , operations =
          [ "replay filer events as S3 puts"
          , "Object Lock retention on the target"
          , "monthly restore drill with a tcfs decrypt"
          ]
        , lagBound = Some "p95 under 24 h (BL-2 T1, proposed)"
        , identity = T.Identity.BucketKey
        , transport = T.Transport.Tls
        , onFailure = [ F.RetryBounded, F.ErrorToCaller ]
        , completeness =
            T.Completeness.Partial
              [ "not seeded; R-C777 preconditions open"
              , "no restore drill of record"
              ]
        }
      , { id = "R-RACK SeaweedFS honey rack to bumble rack (010)"
        , from = "volume-honey"
        , to = "volume-bumble (tank/seaweedfs)"
        , owner = P.Blahaj
        , operations = [ "synchronous replica write per needle" ]
        , lagBound = Some
            "synchronous; tcfs writes fail while bumble is down (R-C778)"
        , identity = T.Identity.BucketKey
        , transport = T.Transport.Tls
        , onFailure = [ F.ErrorToCaller ]
        , completeness =
            T.Completeness.Partial
              [ "inter-server TLS is not configured in blahaj tofu; unverified"
              , "not delivered (blahaj#2219, a GO per act under R-C754)"
              ]
        }
      , { id = "R-CNPG filer metadata to DreamObjects"
        , from = "seaweedfs-filer-pg"
        , to = "DreamObjects"
        , owner = P.Blahaj
        , operations = [ "WAL archive and base backups" ]
        , lagBound = Some "CNPG WAL archive cadence"
        , identity = T.Identity.BucketKey
        , transport = T.Transport.Tls
        , onFailure = [ F.RetryBounded ]
        , completeness =
            T.Completeness.Partial
              [ "metadata alone restores no chunk data; the chunk copy is R-OFFSITE"
              ]
        }
      ]

let _ = assert : Check.interfaceErrors interfaces ≡ ([] : List Text)

let _ = assert : Check.sideErrors replication ≡ ([] : List Text)

in  { interfaces, replication }
