-- expect: L3 an aspirational component stays proposed or absent
-- An odrive-parity extra goes live before the core user story is shown.
let T = ../Types.dhall

let Check = ../Check.dhall

let rows = ../Layers.dhall

let extra =
      { name = "auto-unsync by age"
      , layer = T.Layer.SyncEngine
      , product = T.Product.Tcfs
      , role = T.Role.Aspirational
      , status = T.Status.Live
      , version = None Text
      , concern = T.Concern.FileSync
      , commitPoint = None T.CommitPoint
      , source = "mutant"
      }

in  assert : Check.layerErrors (rows # [ extra ]) ≡ ([] : List Text)
