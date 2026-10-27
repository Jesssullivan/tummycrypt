-- One enrolled workspace root (a Git repo with its linked worktrees, live
-- SQLite files and agent state) across hosts. BL2-Q8 (Linear TIN-3692
-- 36925e2d): "TCFS v1 must cover them". This is a proposed contract, not
-- shipped behaviour.
--
-- The writer lease is part of the state's type: `Active` carries exactly
-- one host, so two writers cannot be written down. A second writer is only
-- representable as `Diverged`, which leaves only through Reconciled.
-- Readers do not take the lease: browsing from honey while sting writes is
-- the Namespace layer's job and changes no state here.
--
-- allStates covers the payload-free states, Active for each host, and
-- Handoff and Diverged for each ordered host pair, so the checks quantify
-- over every state the type admits.
let Host = < Sting | Honey | Neo | Pzm | Bumble | Yoga | Mbp13 >

let hostIndex =
      λ(h : Host) →
        merge
          { Sting = 0
          , Honey = 1
          , Neo = 2
          , Pzm = 3
          , Bumble = 4
          , Yoga = 5
          , Mbp13 = 6
          }
          h

let allHosts =
      [ Host.Sting
      , Host.Honey
      , Host.Neo
      , Host.Pzm
      , Host.Bumble
      , Host.Yoga
      , Host.Mbp13
      ]

let eqN = (./Util.dhall).eqN

let eqHost = λ(a : Host) → λ(b : Host) → eqN (hostIndex a) (hostIndex b)

let Pair = { from : Host, to : Host }

let State =
      < Unenrolled | Quiet | Active : Host | Handoff : Pair | Diverged : Pair >

let Event =
      < Enroll
      | Attach : Host
      | Detach : Host
      | RequestHandoff : Host
      | Quiesced
      | QuiesceFailed
      | LeaseExpired
      | OfflineWrite : Host
      | Reconciled
      | Retire
      >

let S = State

let activeOn =
      λ(a : Host) →
      λ(e : Event) →
        merge
          { Enroll = S.Active a
          , Attach = λ(_ : Host) → S.Active a
          , Detach = λ(h : Host) → if eqHost h a then S.Quiet else S.Active a
          , RequestHandoff =
              λ(h : Host) →
                if    eqHost h a
                then  S.Active a
                else  S.Handoff { from = a, to = h }
          , Quiesced = S.Active a
          , QuiesceFailed = S.Active a
          , LeaseExpired = S.Quiet
          , OfflineWrite =
              λ(h : Host) →
                if    eqHost h a
                then  S.Active a
                else  S.Diverged { from = a, to = h }
          , Reconciled = S.Active a
          , Retire = S.Active a
          }
          e

let inHandoff =
      λ(p : Pair) →
      λ(e : Event) →
        merge
          { Enroll = S.Handoff p
          , Attach = λ(_ : Host) → S.Handoff p
          , Detach = λ(_ : Host) → S.Handoff p
          , RequestHandoff = λ(_ : Host) → S.Handoff p
          , Quiesced = S.Active p.to
          , QuiesceFailed = S.Active p.from
          , LeaseExpired = S.Quiet
          , OfflineWrite =
              λ(h : Host) →
                if    eqHost h p.from
                then  S.Handoff p
                else  S.Diverged { from = p.from, to = h }
          , Reconciled = S.Handoff p
          , Retire = S.Handoff p
          }
          e

let step =
      λ(s : State) →
      λ(e : Event) →
        merge
          { Unenrolled =
              merge
                { Enroll = S.Quiet
                , Attach = λ(_ : Host) → S.Unenrolled
                , Detach = λ(_ : Host) → S.Unenrolled
                , RequestHandoff = λ(_ : Host) → S.Unenrolled
                , Quiesced = S.Unenrolled
                , QuiesceFailed = S.Unenrolled
                , LeaseExpired = S.Unenrolled
                , OfflineWrite = λ(_ : Host) → S.Unenrolled
                , Reconciled = S.Unenrolled
                , Retire = S.Unenrolled
                }
                e
          , Quiet =
              merge
                { Enroll = S.Quiet
                , Attach = λ(h : Host) → S.Active h
                , Detach = λ(_ : Host) → S.Quiet
                , RequestHandoff = λ(h : Host) → S.Active h
                , Quiesced = S.Quiet
                , QuiesceFailed = S.Quiet
                , LeaseExpired = S.Quiet
                , OfflineWrite = λ(h : Host) → S.Active h
                , Reconciled = S.Quiet
                , Retire = S.Unenrolled
                }
                e
          , Active = λ(a : Host) → activeOn a e
          , Handoff = λ(p : Pair) → inHandoff p e
          , Diverged =
              λ(p : Pair) →
                merge
                  { Enroll = S.Diverged p
                  , Attach = λ(_ : Host) → S.Diverged p
                  , Detach = λ(_ : Host) → S.Diverged p
                  , RequestHandoff = λ(_ : Host) → S.Diverged p
                  , Quiesced = S.Diverged p
                  , QuiesceFailed = S.Diverged p
                  , LeaseExpired = S.Diverged p
                  , OfflineWrite = λ(_ : Host) → S.Diverged p
                  , Reconciled = S.Quiet
                  , Retire = S.Diverged p
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

let concatMap = (./Util.dhall).concatMap

let pairs =
      concatMap
        Host
        Pair
        ( λ(a : Host) →
            List/fold
              Host
              allHosts
              (List Pair)
              ( λ(b : Host) →
                λ(acc : List Pair) →
                  [ { from = a, to = b } ] # acc
              )
              ([] : List Pair)
        )
        allHosts

let allStates =
        [ S.Unenrolled, S.Quiet ]
      # concatMap Host State (λ(h : Host) → [ S.Active h ]) allHosts
      # concatMap Pair State (λ(p : Pair) → [ S.Handoff p, S.Diverged p ]) pairs

let allEvents =
        [ Event.Enroll
        , Event.Quiesced
        , Event.QuiesceFailed
        , Event.LeaseExpired
        , Event.Reconciled
        , Event.Retire
        ]
      # concatMap
          Host
          Event
          ( λ(h : Host) →
              [ Event.Attach h
              , Event.Detach h
              , Event.RequestHandoff h
              , Event.OfflineWrite h
              ]
          )
          allHosts

in  { Host
    , hostIndex
    , allHosts
    , eqHost
    , Pair
    , State
    , Event
    , step
    , run
    , pairs
    , allStates
    , allEvents
    }
