-- expect: I1 each pair of adjacent layers has exactly one interface
-- The workspace contract is left out instead of being marked Open.
let T = ../Types.dhall

let Check = ../Check.dhall

let I = ../Interfaces.dhall

let keep =
      λ(i : T.Interface) →
            Natural/isZero (Natural/subtract 5 (T.layerIndex i.lower)) == False
        ||  Natural/isZero (Natural/subtract (T.layerIndex i.lower) 5) == False

let U = ../Util.dhall

in    assert
    :   Check.interfaceErrors (U.filter T.Interface keep I.interfaces)
      ≡ ([] : List Text)
