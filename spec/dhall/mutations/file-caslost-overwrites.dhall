-- expect: F4 a lost compare-and-swap never yields a clean file
-- The publisher ignores a lost race and calls its copy clean.
let File = ../File.dhall

let Check = ../Check.dhall

let isCasLost =
      λ(e : File.Event) →
            Natural/isZero (Natural/subtract 7 (File.eventIndex e))
        &&  Natural/isZero (Natural/subtract (File.eventIndex e) 7)

let mutant =
      λ(s : File.State) →
      λ(e : File.Event) →
        if    Check.eqS s File.State.Publishing && isCasLost e
        then  File.State.Clean
        else  File.step s e

in  assert : Check.fileErrors mutant ≡ ([] : List Text)
