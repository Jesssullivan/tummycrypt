-- Blob topology types (BL2-Q9, Linear TIN-3692 comment 36925e2d, Jess
-- 2026-10-27). The stack runs bottom to top: Media, Volume, ObjectStore,
-- Namespace, SyncEngine, Surface, Workspace. Replication between object
-- stores is a side interface, not a layer. Lake, table and vector stores sit
-- in the Namespace layer as siblings of the TCFS index: each one is a commit
-- point over blobs.
--
-- Roles. Own: the product builds and answers for it. Interim: owns it until
-- a named successor ships. Leverage: adopted, not built. Sibling: shares a
-- lower layer and never runs over TCFS. Baseline: a benchmark yardstick only
-- (BL2-Q9 "No bridge"). Fallback: ruled standby. Excluded: ruled out.
-- Aspirational: named by Jess, waits for the core user story.
--
-- Concern: what a component is for. Ownership is unique per concern (L2).
-- CommitPoint: where a namespace makes a change durable and ordered.
-- Completeness: incompleteness is data, not silence. A Partial interface
-- names each gap with the question or ticket that owns it.
let Layer =
      < Media
      | Volume
      | ObjectStore
      | Namespace
      | SyncEngine
      | Surface
      | Workspace
      >

let layerIndex =
      λ(l : Layer) →
        merge
          { Media = 0
          , Volume = 1
          , ObjectStore = 2
          , Namespace = 3
          , SyncEngine = 4
          , Surface = 5
          , Workspace = 6
          }
          l

let allLayers =
      [ Layer.Media
      , Layer.Volume
      , Layer.ObjectStore
      , Layer.Namespace
      , Layer.SyncEngine
      , Layer.Surface
      , Layer.Workspace
      ]

let Product =
      < Tcfs
      | Bulkload
      | Softconnect
      | Blahaj
      | Lab
      | Apple
      | Linux
      | Upstream
      | Vendor
      >

let Role =
      < Own
      | Interim
      | Leverage
      | Sibling
      | Baseline
      | Fallback
      | Excluded
      | Aspirational
      >

let Status = < Live | Paused | Ruled | Proposed | Absent | Retired >

let Concern =
      < DiskMedia
      | HostVolumes
      | ClusterVolumes
      | NixStoreHold
      | ObjectStorage
      | FileNamespace
      | ChangeFeed
      | TableCatalog
      | VectorIndex
      | FileSync
      | EstateConsolidation
      | LinuxMount
      | MacMount
      | WindowsMount
      | AgentControl
      | WorkspaceState
      | PathMapping
      >

let concernIndex =
      λ(c : Concern) →
        merge
          { DiskMedia = 0
          , HostVolumes = 1
          , ClusterVolumes = 2
          , NixStoreHold = 3
          , ObjectStorage = 4
          , FileNamespace = 5
          , ChangeFeed = 6
          , TableCatalog = 7
          , VectorIndex = 8
          , FileSync = 9
          , EstateConsolidation = 10
          , LinuxMount = 11
          , MacMount = 12
          , WindowsMount = 13
          , AgentControl = 14
          , WorkspaceState = 15
          , PathMapping = 16
          }
          c

let CommitPoint =
      < ObjectConditionalWrite
      | SqlTransaction
      | Raft
      | EtcdTxn
      | LogAppend
      | None
      >

let Component =
      { name : Text
      , layer : Layer
      , product : Product
      , role : Role
      , status : Status
      , version : Optional Text
      , concern : Concern
      , commitPoint : Optional CommitPoint
      , source : Text
      }

let Consistency = < Strong | CloseToOpen | Eventual | LeaseOrdered | Unstated >

let Conditional = < CreateAndReplace | CreateOnly | None | NotApplicable >

let Identity =
      < VolumeUuid
      | BucketKey
      | DeviceKey
      | DeviceKeyAndLease
      | OsUser
      | NotApplicable
      >

let Transport = < Tls | Tailnet | LocalOnly | Plaintext >

let Failure =
      < Hold
      | ErrorToCaller
      | RetryBounded
      | KeepBoth
      | Refuse
      | AbsenceIsNotDeletion
      | Unstated
      >

let Completeness = < Specified | Partial : List Text | Open : Text >

let Interface =
      { id : Text
      , lower : Layer
      , upper : Layer
      , owner : Product
      , operations : List Text
      , consistency : Consistency
      , conditional : Conditional
      , identity : Identity
      , transport : Transport
      , crossesHosts : Bool
      , onFailure : List Failure
      , metadata : Text
      , completeness : Completeness
      }

let SideInterface =
      { id : Text
      , from : Text
      , to : Text
      , owner : Product
      , operations : List Text
      , lagBound : Optional Text
      , identity : Identity
      , transport : Transport
      , onFailure : List Failure
      , completeness : Completeness
      }

in  { Layer
    , layerIndex
    , allLayers
    , Product
    , Role
    , Status
    , Concern
    , concernIndex
    , CommitPoint
    , Component
    , Consistency
    , Conditional
    , Identity
    , Transport
    , Failure
    , Completeness
    , Interface
    , SideInterface
    }
