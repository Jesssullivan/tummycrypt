-- expect: W2 a writer lease moves only through a handoff
-- Attaching from a second host takes the lease without quiescing the first.
let Wt = ../Worktree.dhall

let Check = ../Check.dhall

let mutant =
      λ(s : Wt.State) →
      λ(e : Wt.Event) →
        merge
          { Unenrolled = Wt.step s e
          , Quiet = Wt.step s e
          , Active =
              λ(_ : Wt.Host) →
                merge
                  { Enroll = Wt.step s e
                  , Attach = λ(h : Wt.Host) → Wt.State.Active h
                  , Detach = λ(_ : Wt.Host) → Wt.step s e
                  , RequestHandoff = λ(_ : Wt.Host) → Wt.step s e
                  , Quiesced = Wt.step s e
                  , QuiesceFailed = Wt.step s e
                  , LeaseExpired = Wt.step s e
                  , OfflineWrite = λ(_ : Wt.Host) → Wt.step s e
                  , Reconciled = Wt.step s e
                  , Retire = Wt.step s e
                  }
                  e
          , Handoff = λ(_ : Wt.Pair) → Wt.step s e
          , Diverged = λ(_ : Wt.Pair) → Wt.step s e
          }
          s

in  assert : Check.worktreeErrors mutant ≡ ([] : List Text)
