-- expect: I2 the object store gives the namespace strong reads
-- A store without replace-if-match under the TCFS index.
let T = ../Types.dhall

let Check = ../Check.dhall

let I = ../Interfaces.dhall

let weaken =
      λ(i : T.Interface) →
        if        Natural/isZero (Natural/subtract 2 (T.layerIndex i.lower))
              &&  Natural/isZero (Natural/subtract (T.layerIndex i.lower) 2)
        then  i ⫽ { conditional = T.Conditional.CreateOnly }
        else  i

let mutated =
      List/fold
        T.Interface
        I.interfaces
        (List T.Interface)
        (λ(i : T.Interface) → λ(acc : List T.Interface) → [ weaken i ] # acc)
        ([] : List T.Interface)

in  assert : Check.interfaceErrors mutated ≡ ([] : List Text)
