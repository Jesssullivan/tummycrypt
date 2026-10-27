-- The functional model: both machines meet their rules, and the core user
-- story runs as written. Jess, Linear TIN-3692 36925e2d: "work on any
-- porject, say on honey or neo where the native dev machie is sting".
--
-- File runs: browse then hydrate; edit and publish; lose a race and keep
-- both; resolve and republish; go offline mid-publish; a stale LIST deletes
-- nothing; unsync frees a clean file and refuses a dirty one.
-- Worktree runs: sting to honey to neo and back to sting; browse from honey
-- without a lease; a failed quiesce keeps the source; the source vanishes
-- mid-handoff (the neo Glyph case) and its unpublished files stay Dirty on
-- that host (F2); an offline laptop writes while sting holds the lease and
-- stays apart until reconciled.
let File = ./File.dhall

let Wt = ./Worktree.dhall

let Check = ./Check.dhall

let FS = File.State

let FE = File.Event

let WS = Wt.State

let WE = Wt.Event

let H = Wt.Host

let _ = assert : Check.fileErrors File.step ≡ ([] : List Text)

let _ = assert : Check.worktreeErrors Wt.step ≡ ([] : List Text)

let _ = assert : File.run FS.Absent [ FE.Enumerated ] ≡ FS.Stub

let _ =
        assert
      : File.run FS.Absent [ FE.Enumerated, FE.Open, FE.FetchOk ] ≡ FS.Clean

let _ =
        assert
      : File.run FS.Clean [ FE.LocalWrite, FE.Flush, FE.CasWon ] ≡ FS.Clean

let _ =
        assert
      : File.run FS.Clean [ FE.LocalWrite, FE.Flush, FE.CasLost ] ≡ FS.Conflict

let _ =
        assert
      :   File.run
            FS.Clean
            [ FE.LocalWrite
            , FE.Flush
            , FE.CasLost
            , FE.Resolve
            , FE.Flush
            , FE.CasWon
            ]
        ≡ FS.Clean

let _ =
        assert
      : File.run FS.Clean [ FE.LocalWrite, FE.Flush, FE.Unreachable ] ≡ FS.Dirty

let _ = assert : File.run FS.Clean [ FE.RemoteMissing ] ≡ FS.Clean

let _ = assert : File.run FS.Clean [ FE.Unsync ] ≡ FS.Stub

let _ = assert : File.run FS.Dirty [ FE.Unsync, FE.RemoteTombstone ] ≡ FS.Dirty

let start = [ WE.Enroll, WE.Attach H.Sting ]

let _ =
        assert
      :   Wt.run
            WS.Unenrolled
            (start # [ WE.RequestHandoff H.Honey, WE.Quiesced ])
        ≡ WS.Active H.Honey

let _ =
        assert
      :   Wt.run
            WS.Unenrolled
            (   start
              # [ WE.RequestHandoff H.Honey
                , WE.Quiesced
                , WE.RequestHandoff H.Neo
                , WE.Quiesced
                , WE.RequestHandoff H.Sting
                , WE.Quiesced
                , WE.Detach H.Sting
                ]
            )
        ≡ WS.Quiet

let _ =
        assert
      : Wt.run WS.Unenrolled (start # [ WE.Attach H.Honey ]) ≡ WS.Active H.Sting

let _ =
        assert
      :   Wt.run
            WS.Unenrolled
            (start # [ WE.RequestHandoff H.Honey, WE.QuiesceFailed ])
        ≡ WS.Active H.Sting

let _ =
        assert
      :   Wt.run
            WS.Unenrolled
            [ WE.Enroll
            , WE.Attach H.Neo
            , WE.RequestHandoff H.Sting
            , WE.LeaseExpired
            ]
        ≡ WS.Quiet

let _ =
        assert
      :   Wt.run WS.Unenrolled (start # [ WE.OfflineWrite H.Neo ])
        ≡ WS.Diverged { from = H.Sting, to = H.Neo }

let _ =
        assert
      :   Wt.run
            WS.Unenrolled
            (start # [ WE.OfflineWrite H.Neo, WE.LeaseExpired, WE.Reconciled ])
        ≡ WS.Quiet

let _ = assert : Check.quiesceReady [ FS.Clean, FS.Stub, FS.Absent ] ≡ True

let _ = assert : Check.quiesceReady [ FS.Clean, FS.Dirty ] ≡ False

let _ = assert : Check.quiesceReady [ FS.Hydrating ] ≡ False

in  { file = File, worktree = Wt }
