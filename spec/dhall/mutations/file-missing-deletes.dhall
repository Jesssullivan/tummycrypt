-- expect: F1 a missing index entry is not deletion authority
-- A stale LIST (Missing) removes a clean local copy.
let File = ../File.dhall

let Check = ../Check.dhall

let mutant =
      λ(s : File.State) →
      λ(e : File.Event) →
        if        Check.eqS s File.State.Clean
              &&  Natural/isZero (Natural/subtract 10 (File.eventIndex e))
              &&  Natural/isZero (Natural/subtract (File.eventIndex e) 10)
        then  File.State.Absent
        else  File.step s e

in  assert : Check.fileErrors mutant ≡ ([] : List Text)
