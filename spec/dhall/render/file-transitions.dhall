-- Renders File.step as a Markdown table: one row per state, one column per
-- event. spec/check-dhall.sh compares it with generated/file-transitions.md.
let File = ../File.dhall

let showState =
      λ(s : File.State) →
        merge
          { Absent = "Absent"
          , Stub = "Stub"
          , Hydrating = "Hydrating"
          , Clean = "Clean"
          , Dirty = "Dirty"
          , Publishing = "Publishing"
          , Conflict = "Conflict"
          }
          s

let showEvent =
      λ(e : File.Event) →
        merge
          { Enumerated = "Enumerated"
          , Open = "Open"
          , FetchOk = "FetchOk"
          , Unreachable = "Unreachable"
          , LocalWrite = "LocalWrite"
          , Flush = "Flush"
          , CasWon = "CasWon"
          , CasLost = "CasLost"
          , RemoteUpdated = "RemoteUpdated"
          , RemoteTombstone = "RemoteTombstone"
          , RemoteMissing = "RemoteMissing"
          , Unsync = "Unsync"
          , Resolve = "Resolve"
          }
          e

let cells =
      λ(f : File.Event → Text) →
        List/fold
          File.Event
          File.allEvents
          Text
          (λ(e : File.Event) → λ(acc : Text) → " ${f e} |" ++ acc)
          ""

let header = "| state |" ++ cells showEvent

let rule = "| --- |" ++ cells (λ(_ : File.Event) → "---")

let row =
      λ(s : File.State) →
            "| ${showState s} |"
        ++  cells (λ(e : File.Event) → showState (File.step s e))

let rows =
      List/fold
        File.State
        File.allStates
        Text
        (λ(s : File.State) → λ(acc : Text) → row s ++ "\n" ++ acc)
        ""

in  ''
    # File machine transitions

    Generated from spec/dhall/File.dhall by spec/check-dhall.sh --write. Do not
    edit by hand. Each cell is the state after the event.

    ${header}
    ${rule}
    ${rows}''
