import Audit.Generators

open Plausible model

instance : Arbitrary Table := ⟨genTable "t" "u"⟩
instance : Shrinkable Table := ⟨fun _ => []⟩
