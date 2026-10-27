-- expect: L4 the TCFS namespace commits by a conditional write
-- TCFS grows a metadata database as its commit point.
let T = ../Types.dhall

let Check = ../Check.dhall

let rows = ../Layers.dhall

let db =
      { name = "tcfs metadata database"
      , layer = T.Layer.Namespace
      , product = T.Product.Tcfs
      , role = T.Role.Own
      , status = T.Status.Proposed
      , version = None Text
      , concern = T.Concern.ChangeFeed
      , commitPoint = Some T.CommitPoint.SqlTransaction
      , source = "mutant"
      }

in  assert : Check.layerErrors (rows # [ db ]) ≡ ([] : List Text)
