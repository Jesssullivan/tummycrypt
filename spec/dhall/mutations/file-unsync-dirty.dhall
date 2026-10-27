-- expect: F2 bytes that exist only on this host
-- Unsync drops unpublished edits and leaves a stub.
let File = ../File.dhall

let Check = ../Check.dhall

let isUnsync =
      λ(e : File.Event) →
            Natural/isZero (Natural/subtract 11 (File.eventIndex e))
        &&  Natural/isZero (Natural/subtract (File.eventIndex e) 11)

let mutant =
      λ(s : File.State) →
      λ(e : File.Event) →
        if    Check.eqS s File.State.Dirty && isUnsync e
        then  File.State.Stub
        else  File.step s e

in  assert : Check.fileErrors mutant ≡ ([] : List Text)
