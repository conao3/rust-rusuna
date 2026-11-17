import Assay
import LeanRustGen.Embed.rusuna.h19a5e6a0406f9184.model

open Plausible model

deriving instance DecidableEq for Except

deriving instance Arbitrary, Shrinkable for Value, Direction, Kind

def genOf (xs : List String) : Gen String := do
  let i ← Gen.choose Nat 0 (xs.length - 1) (by omega)
  pure (xs.getD i.val "")

def genList (most : Nat) (gen : Gen α) : Gen (List α) := do
  let n ← Gen.choose Nat 0 most (by omega)
  (List.range n.val).mapM fun _ => gen

def genColumn : Gen String := genOf ["id", "a", "b\"c", ""]

def genScan : Gen Scan := do
  let schema ← genOf ["public", "s"]
  let table ← genOf ["t", "u"]
  let conds ← genList 2 (do pure ((← genColumn), (← Arbitrary.arbitrary)))
  let order ← genList 2 (do pure ((← genColumn), (← Arbitrary.arbitrary)))
  let limit ← Arbitrary.arbitrary
  pure { schema, table, conds, order, limit }

def genNode : Nat → Gen Node
  | 0 => do pure (.column (← genColumn) (← genColumn))
  | fuel + 1 => do
    if (← Gen.choose Nat 0 2 (by omega)).val = 0 then pure (.column (← genColumn) (← genColumn))
    else
      pure (.relation (← genColumn) (← Arbitrary.arbitrary) (← genScan) (← genList 2 genColumn)
        (← genList 2 genColumn) (← genList 3 (genNode fuel)))

instance : Arbitrary Scan := ⟨genScan⟩
instance : Shrinkable Scan := ⟨fun _ => []⟩
instance : Arbitrary Node := ⟨genNode 3⟩
instance : Shrinkable Node := ⟨fun _ => []⟩
instance : Arbitrary Query := ⟨do pure { scan := (← genScan), children := (← genList 3 (genNode 3)) }⟩
instance : Shrinkable Query := ⟨fun _ => []⟩

def genKeys : Gen (List String) := do
  if (← Gen.choose Nat 0 7 (by omega)).val = 0 then genList 2 genColumn else pure ["id"]

def genTable (name other : String) : Gen Table := do
  let extra ← genList 1 (do pure { field := (← genOf ["a", "c"]), column := (← genColumn) })
  let relations ← genList 1 (do
    pure {
      field := (← genOf ["r", "s"]), kind := (← Arbitrary.arbitrary)
      target := (← genOf [other, name, "missing"])
      parent := (← genKeys), child := (← genKeys) })
  pure {
    name, schema := "public", table := (← genOf ["t", "u"])
    columns := [{ field := "a", column := "id" }, { field := "b", column := "b\"c" }] ++ extra
    relations := [
      { field := "r", kind := .object, target := other, parent := (← genKeys), child := (← genKeys) },
      { field := "s", kind := .array, target := other, parent := (← genKeys), child := (← genKeys) }]
      ++ relations }

instance : Arbitrary Schema := ⟨do pure { tables := [← genTable "t" "u", ← genTable "u" "t"] }⟩
instance : Shrinkable Schema := ⟨fun _ => []⟩

partial def genSelection (fuel index : Nat) : Gen Selection := do
  let name ← genOf (if fuel = 0 then ["a", "a", "a", "b", "b", "c", "r", "zz"] else ["a", "a", "b", "c", "r", "r", "s", "s", "zz"])
  let alias ← genOf [name ++ toString index, name ++ toString index, name ++ toString index, name, "x", "%", ""]
  let relation := name = "r" || name = "s"
  let bare := (← Gen.choose Nat 0 9 (by omega)).val != 0
  let conds ← genList 2 (do pure { field := (← genOf ["a", "a", "b", "r", "zz"]), value := (← Arbitrary.arbitrary) })
  let order ← genList 2 (do pure { field := (← genOf ["a", "a", "b", "s"]), direction := (← Arbitrary.arbitrary) })
  let limit ← Arbitrary.arbitrary
  let plain := if relation then name = "r" && bare else bare
  let children ← match fuel with
    | 0 => pure []
    | fuel + 1 =>
      if relation || !bare then do
        let n ← Gen.choose Nat (if bare then 1 else 0) 3 (by split <;> omega)
        (List.range n.val).mapM fun i => genSelection fuel i
      else pure []
  pure {
    alias, name, conds := if plain then [] else conds, order := if plain then [] else order
    limit := if plain then none else limit, children }

def genRoot : Gen Selection := do
  let selection ← genSelection 3 0
  let n ← Gen.choose Nat 0 3 (by omega)
  let children ← (List.range n.val).mapM fun i => genSelection 2 i
  pure { selection with name := (← genOf ["t", "t", "t", "u", "u", "zz"]), children }

instance : Arbitrary Selection := ⟨genRoot⟩
instance : Shrinkable Selection := ⟨fun _ => []⟩

#assay condToks scanToks joinToks joinsToks emit compile using compile_params compile_erase placeholders_numbered
#assay resolveConds resolveOrder resolveScan resolveNode resolveNodes resolve run using resolve_values run_params resolve_declared
