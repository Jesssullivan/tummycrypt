-- expect: W3 diverged writes leave only through reconciliation
-- A lease timeout quietly drops one side of a divergence.
let Wt = ../Worktree.dhall

let Check = ../Check.dhall

let mutant =
      λ(s : Wt.State) →
      λ(e : Wt.Event) →
        merge
          { Unenrolled = Wt.step s e
          , Quiet = Wt.step s e
          , Active = λ(_ : Wt.Host) → Wt.step s e
          , Handoff = λ(_ : Wt.Pair) → Wt.step s e
          , Diverged =
              λ(_ : Wt.Pair) →
                merge
                  { Enroll = Wt.step s e
                  , Attach = λ(_ : Wt.Host) → Wt.step s e
                  , Detach = λ(_ : Wt.Host) → Wt.step s e
                  , RequestHandoff = λ(_ : Wt.Host) → Wt.step s e
                  , Quiesced = Wt.step s e
                  , QuiesceFailed = Wt.step s e
                  , LeaseExpired = Wt.State.Quiet
                  , OfflineWrite = λ(_ : Wt.Host) → Wt.step s e
                  , Reconciled = Wt.step s e
                  , Retire = Wt.step s e
                  }
                  e
          }
          s

in  assert : Check.worktreeErrors mutant ≡ ([] : List Text)
