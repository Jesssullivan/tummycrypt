-- expect: I5 the namespace tells the sync engine that absence is not deletion
-- The namespace contract drops the Missing rule.
let T = ../Types.dhall

let Check = ../Check.dhall

let I = ../Interfaces.dhall

let weaken =
      λ(i : T.Interface) →
        if        Natural/isZero (Natural/subtract 3 (T.layerIndex i.lower))
              &&  Natural/isZero (Natural/subtract (T.layerIndex i.lower) 3)
        then  i ⫽ { onFailure = [ T.Failure.RetryBounded ] }
        else  i

let mutated =
      List/fold
        T.Interface
        I.interfaces
        (List T.Interface)
        (λ(i : T.Interface) → λ(acc : List T.Interface) → [ weaken i ] # acc)
        ([] : List T.Interface)

in  assert : Check.interfaceErrors mutated ≡ ([] : List Text)
