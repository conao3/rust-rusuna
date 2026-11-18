import Audit.Writes

open Plausible model

instance : Arbitrary WriteCatalog := ⟨do
  pure { keys := [⟨"t", ["a"], "t_pkey"⟩, ⟨"u", ["a"], "u_pkey"⟩]
         permissions := ["t", "u"].flatMap fun target => [.insert, .update, .delete].map fun kind =>
           ⟨target, "user", kind, ["a", "b"], .and [], .isNull "a" false, []⟩ }⟩

instance : Shrinkable WriteCatalog := ⟨fun _ => []⟩

instance : Arbitrary WriteInput := ⟨do
  let kind ← Arbitrary.arbitrary
  let target ← genOf ["t", "t", "u", "zz"]
  let value ← Arbitrary.arbitrary
  let column ← genOf ["a", "a", "b", "zz"]
  pure { target, conflict := none, kind, single := false, byKey := false, keys := []
         objects := if kind == .insert then [[(column, value)]] else []
         sets := if kind == .update then [(column, value)] else [], increments := []
         conds := none, children := [⟨"affected_rows", "affected_rows", none, [], none, none, []⟩] }⟩

instance : Shrinkable WriteInput := ⟨fun _ => []⟩
instance : Inhabited (Except Error WritePlan) := ⟨.error .invalidFilter⟩
instance : Inhabited (Except Error Compiled) := ⟨.error .invalidFilter⟩

local instance : Arbitrary WritePlan := ⟨do
  let kind ← Arbitrary.arbitrary
  let value ← Arbitrary.arbitrary
  let conds ← genPredicate 2
  let check ← genPredicate 2
  let assignment : Assignment := ⟨"a", "a", value, false, false⟩
  pure { (default : WritePlan) with target := "t", schema := "public", table := "t", kind, conds
    objects := [[assignment], []], assignments := [assignment], check := .abort check
    outputs := [.count "affected_rows", .rows "returning" [.column "a" (.column "a")]] }⟩

#assay writeBodyToks emitWrite using emitWrite_slots
#assay resolveWriteRaw resolveWrite runWrite using resolveWrite_secure runWrite_accepts
