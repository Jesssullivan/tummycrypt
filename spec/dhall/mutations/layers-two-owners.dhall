-- expect: L2 one concern has one owner
-- bulkload and TCFS both claim worktrees and live SQLite as owners.
let T = ../Types.dhall

let Check = ../Check.dhall

let rows = ../Layers.dhall

let claim =
      { name = "bulkload fold and reattach verbs for worktrees"
      , layer = T.Layer.Workspace
      , product = T.Product.Bulkload
      , role = T.Role.Own
      , status = T.Status.Ruled
      , version = None Text
      , concern = T.Concern.WorkspaceState
      , commitPoint = None T.CommitPoint
      , source = "mutant"
      }

in  assert : Check.layerErrors (rows # [ claim ]) ≡ ([] : List Text)
