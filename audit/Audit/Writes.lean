import Audit.Generators

open Plausible model

deriving instance Arbitrary, Shrinkable for WriteKind

instance : DecidableEq WriteCheck := fun a b => match a, b with
  | .unchecked, .unchecked => isTrue rfl
  | .abort a, .abort b => decidable_of_iff (a = b) (by simp)
  | .unchecked, .abort _ => isFalse (by intro h; cases h)
  | .abort _, .unchecked => isFalse (by intro h; cases h)

instance : Inhabited WriteCheck := ⟨.unchecked⟩

#assay applyWriteCondition applyWriteCheck using applyWriteCondition_exact applyWriteCheck_exact

instance : Inhabited WritePlan := ⟨⟨"", "public", "", "id", [], none, .insert, false, [], [], .and [], .unchecked, []⟩⟩

instance : Arbitrary WritePlan := ⟨do
  let kind ← Arbitrary.arbitrary
  let conds ← genPredicate 2
  let check ← genPredicate 2
  pure { (default : WritePlan) with kind, conds, check := .abort check }⟩

instance : Shrinkable WritePlan := ⟨fun _ => []⟩

#assay applyWritePermission using applyWritePermission_conditions
