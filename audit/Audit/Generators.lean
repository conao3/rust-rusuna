import Assay
import LeanRustGen.Embed.rusuna.h19a5e6a0406f9184.model

open Plausible model

deriving instance DecidableEq for Except

deriving instance Arbitrary, Shrinkable for Value, Direction, Kind, Comparison, AggregateOp, Leaf, RootKind, Operand

def genOf (xs : List String) : Gen String := do
  let i ← Gen.choose Nat 0 (xs.length - 1) (by omega)
  pure (xs.getD i.val "")

def genList (most : Nat) (gen : Gen α) : Gen (List α) := do
  let n ← Gen.choose Nat 0 most (by omega)
  (List.range n.val).mapM fun _ => gen

def genColumn : Gen String := genOf ["id", "a", "b\"c", ""]

def genExpression (genRef : Gen R) : Nat → Gen (Expression R Value)
  | 0 => do
    match (← Gen.choose Nat 0 2 (by omega)).val with
    | 0 => pure (.compare (← genColumn) (← Arbitrary.arbitrary) (← Arbitrary.arbitrary))
    | 1 => pure (.member (← genColumn) (← Arbitrary.arbitrary) (← genList 3 Arbitrary.arbitrary))
    | _ => pure (.isNull (← genColumn) (← Arbitrary.arbitrary))
  | fuel + 1 => do
    match (← Gen.choose Nat 0 6 (by omega)).val with
    | 0 => pure (.and (← genList 2 (genExpression genRef fuel)))
    | 1 => pure (.or (← genList 2 (genExpression genRef fuel)))
    | 2 => pure (.not (← genExpression genRef fuel))
    | 3 => pure (.relation (← genRef) (← genExpression genRef fuel))
    | _ => genExpression genRef 0

def genReference : Gen Reference := do
  pure ⟨"public", (← genOf ["t", "u"]), ["id"], ["id"]⟩

instance : Arbitrary (Expression String Value) := ⟨genExpression (genOf ["r", "s"]) 2⟩
instance : Shrinkable (Expression String Value) := ⟨fun _ => []⟩

mutual
def genPredicate : Nat → Gen Predicate
  | 0 => do
    match (← Gen.choose Nat 0 2 (by omega)).val with
    | 0 => pure (.compare (← genColumn) (← Arbitrary.arbitrary) (← Arbitrary.arbitrary))
    | 1 => pure (.member (← genColumn) (← Arbitrary.arbitrary) (← genList 3 Arbitrary.arbitrary))
    | _ => pure (.isNull (← genColumn) (← Arbitrary.arbitrary))
  | fuel + 1 => do
    match (← Gen.choose Nat 0 6 (by omega)).val with
    | 0 => pure (.and (← genList 2 (genPredicate fuel)))
    | 1 => pure (.or (← genList 2 (genPredicate fuel)))
    | 2 => pure (.not (← genPredicate fuel))
    | 3 => pure (.exists (← genScan fuel) ["id"] ["id"])
    | _ => genPredicate 0
termination_by fuel => (fuel, 0)
def genScan (fuel : Nat) : Gen Scan := do
  let schema ← genOf ["public", "s"]
  let table ← genOf ["t", "u"]
  let conds ← genPredicate fuel
  let order ← genList 2 (do pure ((← genColumn), (← Arbitrary.arbitrary)))
  let limit ← Arbitrary.arbitrary
  pure ⟨(← genOf ["t", "u"]), schema, table, conds, order, limit, (← Arbitrary.arbitrary)⟩
termination_by (fuel, 1)
end

instance : Arbitrary Predicate := ⟨genPredicate 2⟩
instance : Shrinkable Predicate := ⟨fun _ => []⟩

def genNode : Nat → Gen Node
  | 0 => do pure (.column (← genColumn) (.column (← genColumn)))
  | fuel + 1 => do
    if (← Gen.choose Nat 0 2 (by omega)).val = 0 then pure (.column (← genColumn) (.column (← genColumn)))
    else if (← Gen.choose Nat 0 5 (by omega)).val = 0 then
      pure (.group "aggregate" [.column "count" .count])
    else if (← Gen.choose Nat 0 5 (by omega)).val = 0 then
      pure (.nodes "nodes" [] (← Arbitrary.arbitrary) (← genList 2 (genNode fuel)))
    else
      pure (.relation (← genColumn) (← Arbitrary.arbitrary) (← genScan 2) (← genList 2 genColumn)
        (← genList 2 genColumn) (← genList 3 (genNode fuel)))

instance : Inhabited Scan := ⟨.mk "" "" "" (.and []) [] none none⟩
instance : Inhabited (Except Error Scan) := ⟨.error .invalidFilter⟩

instance : Arbitrary Scan := ⟨genScan 2⟩
instance : Shrinkable Scan := ⟨fun _ => []⟩
instance : Arbitrary Node := ⟨genNode 3⟩
instance : Shrinkable Node := ⟨fun _ => []⟩
instance : Arbitrary Query := ⟨do pure { kind := (← Arbitrary.arbitrary), scan := (← genScan 2), children := (← genList 3 (genNode 3)) }⟩
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

instance : DecidableEq Predicate := fun a b => decidable_of_iff (predicateEq a b = true) (predicateEq_iff a b)

instance : DecidableEq Scan
  | .mk n s t c o l f, .mk n' s' t' c' o' l' f' =>
    decidable_of_iff (n = n' ∧ s = s' ∧ t = t' ∧ c = c' ∧ o = o' ∧ l = l' ∧ f = f') (by simp)

instance : Arbitrary Session := ⟨do pure ⟨(← genOf ["user", "other", "admin"]), [("id", ← Arbitrary.arbitrary)]⟩⟩
instance : Shrinkable Session := ⟨fun _ => []⟩

def genPermission (target role : String) : Gen Permission := do
  let value ← if (← Gen.choose Nat 0 1 (by omega)).val = 0 then pure (.session "id") else pure (.literal (← Arbitrary.arbitrary))
  pure ⟨target, role, .compare "a" .eq value, ["a", "b"], true, (← Arbitrary.arbitrary)⟩

instance : Arbitrary Schema := ⟨do pure { tables := [← genTable "t" "u", ← genTable "u" "t"], permissions := [← genPermission "t" "user", ← genPermission "u" "user", ← genPermission "t" "other", ← genPermission "u" "other"], roots := [⟨"t", "t", .list⟩, ⟨"u", "u", .list⟩, ⟨"t_aggregate", "t", .aggregate⟩] }⟩
instance : Shrinkable Schema := ⟨fun _ => []⟩

partial def genSelection (fuel index : Nat) : Gen Selection := do
  let name ← genOf (if fuel = 0 then ["a", "a", "a", "b", "b", "c", "r", "zz"] else ["a", "a", "b", "c", "r", "r", "s", "s", "zz"])
  let alias ← genOf [name ++ toString index, name ++ toString index, name ++ toString index, name, "x", "%", ""]
  let relation := name = "r" || name = "s"
  let bare := (← Gen.choose Nat 0 9 (by omega)).val != 0
  let conds ← genExpression (genOf ["r", "s"]) 2
  let order ← genList 2 (do pure { field := (← genOf ["a", "a", "b", "s"]), direction := (← Arbitrary.arbitrary) })
  let limit ← Arbitrary.arbitrary
  let offset ← Arbitrary.arbitrary
  let plain := if relation then name = "r" && bare else bare
  let children ← match fuel with
    | 0 => pure []
    | fuel + 1 =>
      if relation || !bare then do
        let n ← Gen.choose Nat (if bare then 1 else 0) 3 (by split <;> omega)
        (List.range n.val).mapM fun i => genSelection fuel i
      else pure []
  pure {
    alias, name, conds := if plain then none else some conds, order := if plain then [] else order
    limit := if plain then none else limit, offset := if plain then none else offset, children }

def genRoot : Gen Selection := do
  let selection ← genSelection 3 0
  let n ← Gen.choose Nat 0 3 (by omega)
  let children ← (List.range n.val).mapM fun i => genSelection 2 i
  if (← Gen.choose Nat 0 4 (by omega)).val = 0 then
    pure { selection with name := "t_aggregate", children := [
      ⟨"aggregate", "aggregate", none, [], none, none, [⟨"count", "count", none, [], none, none, []⟩]⟩,
      ⟨"nodes", "nodes", none, [], none, none, [⟨"a", "a", none, [], none, none, []⟩]⟩] }
  else pure { selection with name := (← genOf ["t", "t", "t", "u", "u", "zz"]), children }

instance : Arbitrary Selection := ⟨genRoot⟩
instance : Shrinkable Selection := ⟨fun _ => []⟩
