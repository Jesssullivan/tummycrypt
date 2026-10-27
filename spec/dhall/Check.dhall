-- The rules the topology and the two machines must meet. Each function
-- returns the broken rules as text, so a valid spec is
-- `assert : Check.x value ≡ ([] : List Text)` and a failing assert names the
-- rule. Rule ids: L layers, I interfaces, R replication, F file machine,
-- W worktree machine.
--
-- quiesceReady links the two machines: a handoff may report Quiesced only
-- when no file of the root holds bytes that exist only on the source and
-- nothing is in flight there.
let T = ./Types.dhall

let U = ./Util.dhall

let File = ./File.dhall

let Wt = ./Worktree.dhall

let all = U.all

let any = U.any

let when = U.when

let eqN = U.eqN

let roleIndex =
      λ(r : T.Role) →
        merge
          { Own = 0
          , Interim = 1
          , Leverage = 2
          , Sibling = 3
          , Baseline = 4
          , Fallback = 5
          , Excluded = 6
          , Aspirational = 7
          }
          r

let statusIndex =
      λ(s : T.Status) →
        merge
          { Live = 0
          , Paused = 1
          , Ruled = 2
          , Proposed = 3
          , Absent = 4
          , Retired = 5
          }
          s

let isOwn = λ(c : T.Component) → eqN (roleIndex c.role) 0

let sameLayer =
      λ(a : T.Layer) → λ(b : T.Layer) → eqN (T.layerIndex a) (T.layerIndex b)

let carries =
      λ(c : T.Component) →
            (     eqN (roleIndex c.role) 0
              ||  eqN (roleIndex c.role) 1
              ||  eqN (roleIndex c.role) 2
            )
        &&  (     eqN (statusIndex c.status) 0
              ||  eqN (statusIndex c.status) 1
              ||  eqN (statusIndex c.status) 2
            )

let commitIndex =
      λ(c : T.CommitPoint) →
        merge
          { ObjectConditionalWrite = 0
          , SqlTransaction = 1
          , Raft = 2
          , EtcdTxn = 3
          , LogAppend = 4
          , None = 5
          }
          c

let productIndex =
      λ(p : T.Product) →
        merge
          { Tcfs = 0
          , Bulkload = 1
          , Softconnect = 2
          , Blahaj = 3
          , Lab = 4
          , Apple = 5
          , Linux = 6
          , Upstream = 7
          , Vendor = 8
          }
          p

let layerErrors =
      λ(cs : List T.Component) →
          when
            ( all
                T.Layer
                ( λ(l : T.Layer) →
                    any
                      T.Component
                      (λ(c : T.Component) → sameLayer c.layer l && carries c)
                      cs
                )
                T.allLayers
            )
            "L1 every layer has a live, paused or ruled component that a product owns, holds in the interim or leverages"
        # when
            ( all
                T.Component
                ( λ(a : T.Component) →
                        isOwn a == False
                    ||  eqN
                          ( U.count
                              T.Component
                              ( λ(b : T.Component) →
                                      isOwn b
                                  &&  eqN
                                        (T.concernIndex b.concern)
                                        (T.concernIndex a.concern)
                              )
                              cs
                          )
                          1
                )
                cs
            )
            "L2 one concern has one owner; a second claimant is Interim until it hands over"
        # when
            ( all
                T.Component
                ( λ(c : T.Component) →
                        eqN (roleIndex c.role) 7 == False
                    ||  eqN (statusIndex c.status) 3
                    ||  eqN (statusIndex c.status) 4
                )
                cs
            )
            "L3 an aspirational component stays proposed or absent until the core user story is demonstrated"
        # when
            ( all
                T.Component
                ( λ(c : T.Component) →
                            (     isOwn c
                              &&  eqN (productIndex c.product) 0
                              &&  sameLayer c.layer T.Layer.Namespace
                            )
                        ==  False
                    ||  merge
                          { None = False
                          , Some = λ(p : T.CommitPoint) → eqN (commitIndex p) 0
                          }
                          c.commitPoint
                )
                cs
            )
            "L4 the TCFS namespace commits by a conditional write on the object store and adds no metadata server"

let failureIndex =
      λ(f : T.Failure) →
        merge
          { Hold = 0
          , ErrorToCaller = 1
          , RetryBounded = 2
          , KeepBoth = 3
          , Refuse = 4
          , AbsenceIsNotDeletion = 5
          , Unstated = 6
          }
          f

let isSpecified =
      λ(c : T.Completeness) →
        merge
          { Specified = True
          , Partial = λ(_ : List Text) → False
          , Open = λ(_ : Text) → False
          }
          c

let consistencyStated =
      λ(c : T.Consistency) →
        merge
          { Strong = True
          , CloseToOpen = True
          , Eventual = True
          , LeaseOrdered = True
          , Unstated = False
          }
          c

let isStrong =
      λ(c : T.Consistency) →
        merge
          { Strong = True
          , CloseToOpen = False
          , Eventual = False
          , LeaseOrdered = False
          , Unstated = False
          }
          c

let fullCas =
      λ(c : T.Conditional) →
        merge
          { CreateAndReplace = True
          , CreateOnly = False
          , None = False
          , NotApplicable = False
          }
          c

let protected =
      λ(t : T.Transport) →
        merge
          { Tls = True, Tailnet = True, LocalOnly = True, Plaintext = False }
          t

let between =
      λ(lo : T.Layer) →
      λ(hi : T.Layer) →
      λ(i : T.Interface) →
        sameLayer i.lower lo && sameLayer i.upper hi

let adjacent =
      [ { lo = T.Layer.Media, hi = T.Layer.Volume }
      , { lo = T.Layer.Volume, hi = T.Layer.ObjectStore }
      , { lo = T.Layer.ObjectStore, hi = T.Layer.Namespace }
      , { lo = T.Layer.Namespace, hi = T.Layer.SyncEngine }
      , { lo = T.Layer.SyncEngine, hi = T.Layer.Surface }
      , { lo = T.Layer.Surface, hi = T.Layer.Workspace }
      ]

let interfaceErrors =
      λ(is : List T.Interface) →
          when
            ( all
                { lo : T.Layer, hi : T.Layer }
                ( λ(p : { lo : T.Layer, hi : T.Layer }) →
                    eqN (U.count T.Interface (between p.lo p.hi) is) 1
                )
                adjacent
            )
            "I1 each pair of adjacent layers has exactly one interface"
        # when
            ( all
                T.Interface
                ( λ(i : T.Interface) →
                        between T.Layer.ObjectStore T.Layer.Namespace i == False
                    ||  isStrong i.consistency && fullCas i.conditional
                )
                is
            )
            "I2 the object store gives the namespace strong reads and both create-if-absent and replace-if-match"
        # when
            ( all
                T.Interface
                ( λ(i : T.Interface) →
                        isSpecified i.completeness == False
                    ||      consistencyStated i.consistency
                        &&      Natural/isZero
                                  (List/length T.Failure i.onFailure)
                            ==  False
                        &&  all
                              T.Failure
                              ( λ(f : T.Failure) →
                                  eqN (failureIndex f) 6 == False
                              )
                              i.onFailure
                )
                is
            )
            "I3 a specified interface states its consistency and every failure mode; gaps are Partial or Open with an owner"
        # when
            ( all
                T.Interface
                ( λ(i : T.Interface) →
                    i.crossesHosts == False || protected i.transport
                )
                is
            )
            "I4 an interface that crosses hosts is encrypted in transit"
        # when
            ( all
                T.Interface
                ( λ(i : T.Interface) →
                        between T.Layer.Namespace T.Layer.SyncEngine i == False
                    ||  any
                          T.Failure
                          (λ(f : T.Failure) → eqN (failureIndex f) 5)
                          i.onFailure
                )
                is
            )
            "I5 the namespace tells the sync engine that absence is not deletion"

let sideErrors =
      λ(rs : List T.SideInterface) →
        when
          ( all
              T.SideInterface
              ( λ(r : T.SideInterface) →
                      merge
                        { None = False, Some = λ(_ : Text) → True }
                        r.lagBound
                  &&  protected r.transport
              )
              rs
          )
          "R1 a replication leg states its lag bound and is encrypted in transit"

let eqS =
      λ(a : File.State) →
      λ(b : File.State) →
        eqN (File.stateIndex a) (File.stateIndex b)

let eqE =
      λ(a : File.Event) →
      λ(b : File.Event) →
        eqN (File.eventIndex a) (File.eventIndex b)

let FileStep = File.State → File.Event → File.State

let forAllFile =
      λ(p : File.State → File.Event → Bool) →
        all
          File.State
          ( λ(s : File.State) →
              all File.Event (λ(e : File.Event) → p s e) File.allEvents
          )
          File.allStates

let fileErrors =
      λ(step : FileStep) →
          when
            ( all
                File.State
                (λ(s : File.State) → eqS (step s File.Event.RemoteMissing) s)
                File.allStates
            )
            "F1 a missing index entry is not deletion authority; only a tombstone is"
        # when
            ( forAllFile
                ( λ(s : File.State) →
                  λ(e : File.Event) →
                        File.holdsLocalOnlyBytes s == False
                    ||  File.holdsLocalOnlyBytes (step s e)
                    ||  eqS s File.State.Publishing && eqE e File.Event.CasWon
                )
            )
            "F2 bytes that exist only on this host leave that state only by a won publish"
        # when
            (     eqS (step File.State.Clean File.Event.Unsync) File.State.Stub
              &&  eqS
                    (step File.State.Hydrating File.Event.Unsync)
                    File.State.Stub
            )
            "F3 unsync turns a clean or hydrating file back into a stub"
        # when
            ( all
                File.State
                ( λ(s : File.State) →
                        eqS s File.State.Clean
                    ||      eqS (step s File.Event.CasLost) File.State.Clean
                        ==  False
                )
                File.allStates
            )
            "F4 a lost compare-and-swap never yields a clean file; the loser keeps both"
        # when
            ( all
                File.State
                ( λ(s : File.State) →
                    File.inFlight (step s File.Event.Unreachable) == False
                )
                File.allStates
            )
            "F5 an unreachable store ends every in-flight operation in a resting state"
        # when
            (     eqS
                    (step File.State.Stub File.Event.Open)
                    File.State.Hydrating
              &&  eqS
                    (step File.State.Hydrating File.Event.FetchOk)
                    File.State.Clean
              &&  eqS
                    (step File.State.Absent File.Event.Enumerated)
                    File.State.Stub
            )
            "F6 names appear before content, and a read hydrates"

let key =
      λ(s : Wt.State) →
        merge
          { Unenrolled = 0
          , Quiet = 1
          , Active = λ(h : Wt.Host) → 10 + Wt.hostIndex h
          , Handoff =
              λ(p : Wt.Pair) →
                100 + 10 * Wt.hostIndex p.from + Wt.hostIndex p.to
          , Diverged =
              λ(p : Wt.Pair) →
                200 + 10 * Wt.hostIndex p.from + Wt.hostIndex p.to
          }
          s

let eqW = λ(a : Wt.State) → λ(b : Wt.State) → eqN (key a) (key b)

let activeHost =
      λ(s : Wt.State) →
        merge
          { Unenrolled = None Wt.Host
          , Quiet = None Wt.Host
          , Active = λ(h : Wt.Host) → Some h
          , Handoff = λ(_ : Wt.Pair) → None Wt.Host
          , Diverged = λ(_ : Wt.Pair) → None Wt.Host
          }
          s

let isReconciled =
      λ(e : Wt.Event) →
        merge
          { Enroll = False
          , Attach = λ(_ : Wt.Host) → False
          , Detach = λ(_ : Wt.Host) → False
          , RequestHandoff = λ(_ : Wt.Host) → False
          , Quiesced = False
          , QuiesceFailed = False
          , LeaseExpired = False
          , OfflineWrite = λ(_ : Wt.Host) → False
          , Reconciled = True
          , Retire = False
          }
          e

let eventTag =
      λ(e : Wt.Event) →
        merge
          { Enroll = 0
          , Attach = λ(_ : Wt.Host) → 1
          , Detach = λ(_ : Wt.Host) → 2
          , RequestHandoff = λ(_ : Wt.Host) → 3
          , Quiesced = 4
          , QuiesceFailed = 5
          , LeaseExpired = 6
          , OfflineWrite = λ(_ : Wt.Host) → 7
          , Reconciled = 8
          , Retire = 9
          }
          e

let WtStep = Wt.State → Wt.Event → Wt.State

let forAllEvents = λ(p : Wt.Event → Bool) → all Wt.Event p Wt.allEvents

let worktreeErrors =
      λ(step : WtStep) →
          when
            ( all
                Wt.Host
                ( λ(a : Wt.Host) →
                    forAllEvents
                      ( λ(e : Wt.Event) →
                          merge
                            { None = True
                            , Some = λ(b : Wt.Host) → Wt.eqHost a b
                            }
                            (activeHost (step (Wt.State.Active a) e))
                      )
                )
                Wt.allHosts
            )
            "W2 a writer lease moves only through a handoff; no event hands it straight to another host"
        # when
            ( all
                Wt.Pair
                ( λ(p : Wt.Pair) →
                    forAllEvents
                      ( λ(e : Wt.Event) →
                          if    isReconciled e
                          then  eqW
                                  (step (Wt.State.Diverged p) e)
                                  Wt.State.Quiet
                          else  eqW
                                  (step (Wt.State.Diverged p) e)
                                  (Wt.State.Diverged p)
                      )
                )
                Wt.pairs
            )
            "W3 diverged writes leave only through reconciliation, never by timeout or a new attach"
        # when
            ( all
                Wt.Pair
                ( λ(p : Wt.Pair) →
                    forAllEvents
                      ( λ(e : Wt.Event) →
                          merge
                            { None = True
                            , Some =
                                λ(h : Wt.Host) →
                                      eqN (eventTag e) 4 && Wt.eqHost h p.to
                                  ||  eqN (eventTag e) 5 && Wt.eqHost h p.from
                            }
                            (activeHost (step (Wt.State.Handoff p) e))
                      )
                )
                Wt.pairs
            )
            "W4 a handoff completes to its target only when quiesced and falls back to its source only when quiescing fails"
        # when
            ( all
                Wt.State
                ( λ(s : Wt.State) →
                        eqW s Wt.State.Quiet
                    ||  eqW s Wt.State.Unenrolled
                    ||      eqW (step s Wt.Event.Retire) Wt.State.Unenrolled
                        ==  False
                )
                Wt.allStates
            )
            "W5 a root retires only when no host holds or contests its lease"

let quiesceReady =
      λ(files : List File.State) →
        all
          File.State
          ( λ(s : File.State) →
              File.holdsLocalOnlyBytes s == False && File.inFlight s == False
          )
          files

in  { layerErrors
    , interfaceErrors
    , sideErrors
    , fileErrors
    , worktreeErrors
    , quiesceReady
    , eqS
    , eqW
    , key
    }
