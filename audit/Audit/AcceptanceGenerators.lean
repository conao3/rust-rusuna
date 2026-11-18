import Audit.Generators

open Plausible model

instance : Arbitrary Table := ⟨do
  let table ← genTable "t" "u"
  let body ← genBody
  pure { table with body }⟩
instance : Shrinkable Table := ⟨fun _ => []⟩
