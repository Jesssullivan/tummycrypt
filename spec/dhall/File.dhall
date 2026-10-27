-- One path on one enrolled host. `step` is a total function: Dhall's
-- `merge` refuses a missing handler, so every (state, event) pair has a
-- written answer. Check.dhall states the properties over all pairs.
--
-- Remote side, from tcfs-sync index_entry.rs (origin/main 2d5b33e):
-- Live (committed manifest), Deleted (durable tombstone written with
-- compare-and-swap) and Missing (no object, or absent from a LIST). Only a
-- tombstone is deletion authority; Missing is never acted on.
--
-- Pinning is policy, not state: a pinned path refuses Unsync and eviction
-- in the surface before an event reaches this machine.
--
-- holdsLocalOnlyBytes: the bytes exist only on this host (Dirty, Publishing,
-- Conflict), so losing them loses work. inFlight: an operation waits on the
-- object store (Hydrating, Publishing). run folds events left to right.
let State =
      < Absent | Stub | Hydrating | Clean | Dirty | Publishing | Conflict >

let Event =
      < Enumerated
      | Open
      | FetchOk
      | Unreachable
      | LocalWrite
      | Flush
      | CasWon
      | CasLost
      | RemoteUpdated
      | RemoteTombstone
      | RemoteMissing
      | Unsync
      | Resolve
      >

let S = State

let stateIndex =
      λ(s : State) →
        merge
          { Absent = 0
          , Stub = 1
          , Hydrating = 2
          , Clean = 3
          , Dirty = 4
          , Publishing = 5
          , Conflict = 6
          }
          s

let eventIndex =
      λ(e : Event) →
        merge
          { Enumerated = 0
          , Open = 1
          , FetchOk = 2
          , Unreachable = 3
          , LocalWrite = 4
          , Flush = 5
          , CasWon = 6
          , CasLost = 7
          , RemoteUpdated = 8
          , RemoteTombstone = 9
          , RemoteMissing = 10
          , Unsync = 11
          , Resolve = 12
          }
          e

let allStates =
      [ S.Absent
      , S.Stub
      , S.Hydrating
      , S.Clean
      , S.Dirty
      , S.Publishing
      , S.Conflict
      ]

let allEvents =
      [ Event.Enumerated
      , Event.Open
      , Event.FetchOk
      , Event.Unreachable
      , Event.LocalWrite
      , Event.Flush
      , Event.CasWon
      , Event.CasLost
      , Event.RemoteUpdated
      , Event.RemoteTombstone
      , Event.RemoteMissing
      , Event.Unsync
      , Event.Resolve
      ]

let holdsLocalOnlyBytes =
      λ(s : State) →
        merge
          { Absent = False
          , Stub = False
          , Hydrating = False
          , Clean = False
          , Dirty = True
          , Publishing = True
          , Conflict = True
          }
          s

let inFlight =
      λ(s : State) →
        merge
          { Absent = False
          , Stub = False
          , Hydrating = True
          , Clean = False
          , Dirty = False
          , Publishing = True
          , Conflict = False
          }
          s

let step =
      λ(s : State) →
      λ(e : Event) →
        merge
          { Absent =
              merge
                { Enumerated = S.Stub
                , Open = S.Absent
                , FetchOk = S.Absent
                , Unreachable = S.Absent
                , LocalWrite = S.Dirty
                , Flush = S.Absent
                , CasWon = S.Absent
                , CasLost = S.Absent
                , RemoteUpdated = S.Stub
                , RemoteTombstone = S.Absent
                , RemoteMissing = S.Absent
                , Unsync = S.Absent
                , Resolve = S.Absent
                }
                e
          , Stub =
              merge
                { Enumerated = S.Stub
                , Open = S.Hydrating
                , FetchOk = S.Stub
                , Unreachable = S.Stub
                , LocalWrite = S.Hydrating
                , Flush = S.Stub
                , CasWon = S.Stub
                , CasLost = S.Stub
                , RemoteUpdated = S.Stub
                , RemoteTombstone = S.Absent
                , RemoteMissing = S.Stub
                , Unsync = S.Stub
                , Resolve = S.Stub
                }
                e
          , Hydrating =
              merge
                { Enumerated = S.Hydrating
                , Open = S.Hydrating
                , FetchOk = S.Clean
                , Unreachable = S.Stub
                , LocalWrite = S.Hydrating
                , Flush = S.Hydrating
                , CasWon = S.Hydrating
                , CasLost = S.Hydrating
                , RemoteUpdated = S.Hydrating
                , RemoteTombstone = S.Absent
                , RemoteMissing = S.Hydrating
                , Unsync = S.Stub
                , Resolve = S.Hydrating
                }
                e
          , Clean =
              merge
                { Enumerated = S.Clean
                , Open = S.Clean
                , FetchOk = S.Clean
                , Unreachable = S.Clean
                , LocalWrite = S.Dirty
                , Flush = S.Clean
                , CasWon = S.Clean
                , CasLost = S.Clean
                , RemoteUpdated = S.Stub
                , RemoteTombstone = S.Absent
                , RemoteMissing = S.Clean
                , Unsync = S.Stub
                , Resolve = S.Clean
                }
                e
          , Dirty =
              merge
                { Enumerated = S.Dirty
                , Open = S.Dirty
                , FetchOk = S.Dirty
                , Unreachable = S.Dirty
                , LocalWrite = S.Dirty
                , Flush = S.Publishing
                , CasWon = S.Dirty
                , CasLost = S.Dirty
                , RemoteUpdated = S.Dirty
                , RemoteTombstone = S.Dirty
                , RemoteMissing = S.Dirty
                , Unsync = S.Dirty
                , Resolve = S.Dirty
                }
                e
          , Publishing =
              merge
                { Enumerated = S.Publishing
                , Open = S.Publishing
                , FetchOk = S.Publishing
                , Unreachable = S.Dirty
                , LocalWrite = S.Dirty
                , Flush = S.Publishing
                , CasWon = S.Clean
                , CasLost = S.Conflict
                , RemoteUpdated = S.Publishing
                , RemoteTombstone = S.Publishing
                , RemoteMissing = S.Publishing
                , Unsync = S.Publishing
                , Resolve = S.Publishing
                }
                e
          , Conflict =
              merge
                { Enumerated = S.Conflict
                , Open = S.Conflict
                , FetchOk = S.Conflict
                , Unreachable = S.Conflict
                , LocalWrite = S.Conflict
                , Flush = S.Conflict
                , CasWon = S.Conflict
                , CasLost = S.Conflict
                , RemoteUpdated = S.Conflict
                , RemoteTombstone = S.Conflict
                , RemoteMissing = S.Conflict
                , Unsync = S.Conflict
                , Resolve = S.Dirty
                }
                e
          }
          s

let run =
      λ(start : State) →
      λ(events : List Event) →
        List/fold
          Event
          (List/reverse Event events)
          State
          (λ(e : Event) → λ(acc : State) → step acc e)
          start

in  { State
    , Event
    , stateIndex
    , eventIndex
    , allStates
    , allEvents
    , holdsLocalOnlyBytes
    , inFlight
    , step
    , run
    }
