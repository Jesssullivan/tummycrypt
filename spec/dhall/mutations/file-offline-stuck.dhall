-- expect: F5 an unreachable store ends every in-flight operation
-- A hydration waits forever when the store is unreachable.
let File = ../File.dhall

let Check = ../Check.dhall

let isUnreachable =
      λ(e : File.Event) →
            Natural/isZero (Natural/subtract 3 (File.eventIndex e))
        &&  Natural/isZero (Natural/subtract (File.eventIndex e) 3)

let mutant =
      λ(s : File.State) →
      λ(e : File.Event) →
        if    Check.eqS s File.State.Hydrating && isUnreachable e
        then  File.State.Hydrating
        else  File.step s e

in  assert : Check.fileErrors mutant ≡ ([] : List Text)
