-- Small list helpers. The spec imports nothing from the network or the
-- environment (spec/check-dhall.sh rules R1 and R2), so it carries these
-- instead of the Dhall Prelude.
let all =
      λ(a : Type) →
      λ(p : a → Bool) →
      λ(xs : List a) →
        List/fold a xs Bool (λ(x : a) → λ(acc : Bool) → p x && acc) True

let any =
      λ(a : Type) →
      λ(p : a → Bool) →
      λ(xs : List a) →
        List/fold a xs Bool (λ(x : a) → λ(acc : Bool) → p x || acc) False

let concatMap =
      λ(a : Type) →
      λ(b : Type) →
      λ(f : a → List b) →
      λ(xs : List a) →
        List/fold
          a
          xs
          (List b)
          (λ(x : a) → λ(acc : List b) → f x # acc)
          ([] : List b)

let filter =
      λ(a : Type) →
      λ(p : a → Bool) →
      λ(xs : List a) →
        List/fold
          a
          xs
          (List a)
          (λ(x : a) → λ(acc : List a) → if p x then [ x ] # acc else acc)
          ([] : List a)

let count =
      λ(a : Type) →
      λ(p : a → Bool) →
      λ(xs : List a) →
        List/length a (filter a p xs)

let eqN =
      λ(a : Natural) →
      λ(b : Natural) →
            Natural/isZero (Natural/subtract a b)
        &&  Natural/isZero (Natural/subtract b a)

let when =
      λ(ok : Bool) → λ(rule : Text) → if ok then [] : List Text else [ rule ]

in  { all, any, concatMap, filter, count, eqN, when }
