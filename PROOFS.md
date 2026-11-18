# Theorem statements

These statements are copied from the `model` block in `src/compiler.rs`. Definitions
and proof terms there are authoritative. The properties apply to all Lean schemas,
sessions and selections; a successful resolution is required where stated.

## Independent properties

Twelve properties, expressed by twenty boundary statements. `resolve_tables` is a
consequence of the scan condition property, not an additional independent property.
`resolve_permissions`, the intermediate traversal lemmas and the pre-permission
`resolveSelection` lemmas support these same properties.

```lean
theorem run_params (schema : Schema) (session : Session) (root : Selection) :
    match run schema session root with
    | .ok compiled => compiled.params = documentValues schema session root
    | .error _ => True
```

```lean
theorem compile_params (query : Query) : (compile query).params = queryValues query
```

```lean
theorem resolve_values (schema : Schema) (session : Session) (root : Selection) :
    match resolve schema session root with
    | .ok query => queryValues query = documentValues schema session root
    | .error _ => True
```

```lean
theorem compile_erase (query : Query) : (compile (eraseQuery query)).sql = (compile query).sql
```

```lean
theorem placeholders_numbered (query : Query) :
    placeholders query = List.range' 1 (queryValues query).length
```

```lean
theorem slots_numbered (toks : List Tok) :
    slots toks = List.range' 1 (render toks).params.length
```

```lean
theorem unquote_quote (name rest : List Char) (h : rest.head? ≠ some '"') :
    unquoteChars (quoteChars name ++ rest) = some (name, rest)
```

```lean
theorem unquote_quoteChars (name : List Char) : unquoteChars (quoteChars name) = some (name, [])
```

```lean
theorem unquote_quoteString (name rest : List Char) (h : rest.head? ≠ some '\'') :
    unquoteStringChars (quoteStringChars name ++ rest) = some (name, rest)
```

```lean
theorem unquote_quoteStringChars (name : List Char) : unquoteStringChars (quoteStringChars name) = some (name, [])
```

```lean
theorem resolve_declared (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) :
    ∀ scan ∈ allScans query, ∃ table ∈ schema.tables,
      table.name = scan.name ∧ table.schema = scan.schema ∧ table.table = scan.table
```

```lean
theorem distinct_iff (aliases : List String) : distinct aliases = true ↔ aliases.Nodup
```

```lean
theorem resolve_scan_conditions (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ scan ∈ allScans query, ∃ condition,
      roleCondition schema session scan.name = .ok condition ∧
      (scan.conds = condition ∨ ∃ es, scan.conds = .and es ∧ condition ∈ es)
```

```lean
theorem resolve_columns (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ read ∈ queryReads query, ∃ table ∈ schema.tables, table.name = read.1 ∧
      ∃ permission ∈ schema.permissions, permission.role = session.role ∧ permission.target = read.1 ∧
        ∃ field ∈ table.columns, field.field ∈ permission.columns ∧ field.column = read.2
```

```lean
theorem resolve_tables (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ scan ∈ allScans query, ∃ permission ∈ schema.permissions,
      permission.role = session.role ∧ permission.target = scan.name
```

The write properties concern post-write checks that abort the statement, row conditions
on updates and deletes, and writable columns including the separation of user inputs
from presets. The table-permission witness is a consequence of the same guarantee.
`resolveWrite_returning_conditions` extends the existing read-scan property to mutation
outputs; it is not a separate kind of authorization property.

```lean
theorem resolveWrite_permissions (schema : Schema) (catalog : WriteCatalog) (session : Session)
    (input : WriteInput) (plan : WritePlan) (h : resolveWrite schema catalog session input = .ok plan)
    (role : session.role ≠ "admin") :
    ∃ permission, findWritePermission catalog session.role plan.target plan.kind = some permission ∧
      ∃ condition check, writePolicy schema session permission = .ok (condition, check) ∧
        (plan.kind = .insert ∨ plan.conds = condition ∨ ∃ es, plan.conds = .and es ∧ condition ∈ es) ∧
        (plan.kind = .delete ∨ plan.check = .abort check) ∧
        (∀ assignment ∈ writeColumns plan, writeColumnAllowed schema permission assignment = true)
```

```lean
theorem resolveWrite_user_columns (schema : Schema) (catalog : WriteCatalog) (session : Session)
    (input : WriteInput) (plan : WritePlan) (h : resolveWrite schema catalog session input = .ok plan)
    (role : session.role ≠ "admin") :
    ∃ permission, findWritePermission catalog session.role input.target input.kind = some permission ∧
      ∀ value ∈ input.objects.flatten ++ input.sets ++ input.increments,
        value.1 ∈ permission.columns ∧ value.1 ∉ permission.presets.map (·.1)
```

```lean
theorem resolveWrite_tables (schema : Schema) (catalog : WriteCatalog) (session : Session)
    (input : WriteInput) (plan : WritePlan) (h : resolveWrite schema catalog session input = .ok plan)
    (role : session.role ≠ "admin") :
    ∃ permission ∈ catalog.permissions, permission.role = session.role ∧
      permission.target = plan.target ∧ permission.kind = plan.kind
```

```lean
theorem resolveWrite_returning_conditions (schema : Schema) (catalog : WriteCatalog) (session : Session)
    (input : WriteInput) (plan : WritePlan) (h : resolveWrite schema catalog session input = .ok plan)
    (role : session.role ≠ "admin") (alias : String) (nodes : List Node)
    (output : WriteOutput.rows alias nodes ∈ plan.outputs) :
    ∀ scan ∈ (nodesScans nodes).flatMap scanTree, ∃ condition,
      roleCondition schema session scan.name = .ok condition ∧
      (scan.conds = condition ∨ ∃ es, scan.conds = .and es ∧ condition ∈ es)
```

```lean
theorem resolveWrite_declared (schema : Schema) (catalog : WriteCatalog) (session : Session)
    (input : WriteInput) (plan : WritePlan) (h : resolveWrite schema catalog session input = .ok plan) :
    ∃ table ∈ schema.tables, table.name = plan.target ∧ table.schema = plan.schema ∧ table.table = plan.table
```

## Conditions restated next to their implementations

Nineteen boundary statements. The selection acceptance equalities compare success
with a separate Boolean traversal. Their seven expression, ordering, scan and node
lemmas and the existential success form below support the same conditions and are
not counted again.

```lean
theorem aliasValid_iff (alias : String) :
    aliasValid alias = true ↔ alias ≠ "" ∧ alias ≠ rowAlias ∧ alias.utf8ByteSize ≤ 63
```

```lean
theorem selectionValid_iff (aliases : List String) :
    selectionValid aliases = true ↔
      aliases ≠ [] ∧ aliases.length ≤ maxFields ∧ (∀ a, a ∈ aliases → aliasValid a = true)
        ∧ aliases.Nodup
```

```lean
theorem relationValid_iff (relation : Relation) :
    relationValid relation = true ↔
      relation.parent ≠ [] ∧ relation.parent.length = relation.child.length
```

```lean
theorem lateralScan_object (scan : Scan) :
    lateralScan .object scan = ⟨scan.name, scan.schema, scan.table, scan.conds, [], some 1, none⟩
```

```lean
theorem lateralScan_array (scan : Scan) : lateralScan .array scan = scan
```

```lean
theorem lateralScan_aggregate (scan : Scan) : lateralScan .aggregate scan = scan
```

```lean
theorem lateralScan_limit (kind : Kind) (scan : Scan) :
    (lateralScan kind scan).limit = (if kind = .object then some 1 else scan.limit)
```

```lean
theorem scalarValid_iff (v : Value) : scalarValid v = true ↔
    (v != .null && (match v with
      | .float bits => (bits / 4503599627370496) % 2048 != 2047
      | _ => true)) = true
```

```lean
theorem membershipValid_iff (vs : List Value) : membershipValid vs = true ↔
    vs.length ≤ maxInValues ∧ ∀ v ∈ vs, scalarValid v = true
```

```lean
theorem predicateEq_iff (e f : Predicate) : predicateEq e f = true ↔ e = f
```

```lean
theorem predicatesEq_iff (es fs : List Predicate) : predicatesEq es fs = true ↔ es = fs
```

```lean
theorem hasConjunct_iff (condition expression : Predicate) :
    hasConjunct condition expression = true ↔
      expression = condition ∨ ∃ es, expression = .and es ∧ condition ∈ es
```

```lean
theorem addPermission_condition (schema : Schema) (session : Session) (scan : Scan) :
    addPermission schema session scan = (roleCondition schema session scan.name).map
      (fun condition => .mk scan.name scan.schema scan.table (.and [scan.conds, condition]) scan.order scan.limit scan.offset)
```

```lean
theorem appendCondition_exact (scan : Scan) (condition : Predicate) :
    appendCondition scan condition =
      .mk scan.name scan.schema scan.table (.and [scan.conds, condition]) scan.order scan.limit scan.offset
```

```lean
theorem resolveSelection_accepts (schema : Schema) (root : Selection) :
    (resolveSelection schema root).isOk = acceptsSelection schema root
```

```lean
theorem runSelection_accepts (schema : Schema) (root : Selection) :
    (runSelection schema root).isOk = acceptsSelection schema root
```

```lean
theorem applyWriteCondition_exact (kind : WriteKind) (user condition : Predicate) :
    applyWriteCondition kind user condition = (if kind = .insert then user else .and [user, condition])
```

```lean
theorem applyWriteCheck_exact (kind : WriteKind) (check : Predicate) :
    applyWriteCheck kind check = (if kind = .delete then .unchecked else .abort check)
```

```lean
theorem applyWritePermission_conditions (plan : WritePlan) (condition check : Predicate) :
    (applyWritePermission plan condition check).conds = (if plan.kind = .insert then plan.conds else .and [plan.conds, condition]) ∧
    (applyWritePermission plan condition check).check = (if plan.kind = .delete then .unchecked else .abort check)
```

## Checks of individual values

None. Unit tests and fixtures are not counted as theorems.

## Selection acceptance

This checker describes pre-permission resolution. It shares pure name and mode
lookups and the verified scalar, membership, alias and relationship checks with the
resolver. It calls no resolver or exception-returning helper and constructs no
`Query`, `Node` or `Scan`. Child aliases are checked by their containing selection.

```lean
mutual
def acceptsExpression (schema : Schema) (table : Table) (depth : Nat)
    (e : Expression String Value) : Bool :=
  depth ≤ maxFilterDepth && match e with
  | .compare field _ value => (findColumn table field).isSome && scalarValid value
  | .member field _ values => (findColumn table field).isSome && membershipValid values
  | .isNull field _ => (findColumn table field).isSome
  | .and es | .or es => es.length ≤ maxFields && acceptsExpressions schema table (depth + 1) es
  | .not e => acceptsExpression schema table (depth + 1) e
  | .relation name e => match findRelation table name with
    | none => false
    | some relation => decide (relation.kind ≠ .aggregate) && relationValid relation &&
      match findTable schema relation.target with
      | none => false
      | some target => acceptsExpression schema target (depth + 1) e
termination_by sizeOf e
def acceptsExpressions (schema : Schema) (table : Table) (depth : Nat)
    (es : List (Expression String Value)) : Bool :=
  match es with
  | [] => true
  | e :: es => acceptsExpression schema table depth e && acceptsExpressions schema table depth es
termination_by sizeOf es
end

def acceptsFilter (schema : Schema) (table : Table) (conds : Option (Expression String Value)) : Bool :=
  conds.all (acceptsExpression schema table 0)

def acceptsOrder (table : Table) (order : List Order) : Bool :=
  order.all fun o => (findColumn table o.field).isSome

def acceptsScan (schema : Schema) (table : Table) (conds : Option (Expression String Value))
    (order : List Order) : Bool :=
  acceptsFilter schema table conds && acceptsOrder table order

mutual
def acceptsNode (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (selection : Selection) : Bool :=
  let arguments := selection.conds.isNone && selection.order.isEmpty &&
    selection.limit.isNone && selection.offset.isNone
  match modeLeaf table mode selection.name with
  | some _ => arguments && selection.children.isEmpty
  | none => match childMode mode selection.name with
    | some next => depth < maxDepth + 3 && arguments &&
      selectionValid (selection.children.map (·.alias)) &&
      acceptsNodes schema table (depth + 1) next selection.children
    | none => decide (mode = .row) && match findRelation table selection.name with
      | none => false
      | some relation => match findTable schema relation.target with
        | none => false
        | some target => depth < maxDepth && (decide (relation.kind ≠ .object) || arguments) &&
          relationValid relation && selectionValid (selection.children.map (·.alias)) &&
          acceptsScan schema target selection.conds selection.order &&
          acceptsNodes schema target (depth + 1) (relationMode relation.kind) selection.children
termination_by sizeOf selection
decreasing_by all_goals cases selection <;> simp_all <;> omega
def acceptsNodes (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (selections : List Selection) : Bool :=
  match selections with
  | [] => true
  | selection :: selections =>
    acceptsNode schema table depth mode selection && acceptsNodes schema table depth mode selections
termination_by sizeOf selections
end

def acceptsSelection (schema : Schema) (root : Selection) : Bool :=
  match schema.roots.find? (·.field = root.name) with
  | none => false
  | some field => match findTable schema field.target with
    | none => false
    | some table => selectionValid (root.children.map (·.alias)) &&
      acceptsScan schema table root.conds root.order &&
      acceptsNodes schema table 0 (if field.kind = .aggregate then .aggregate else .row) root.children
```

The success statement also has an existential form:

```lean
theorem resolveSelection_ok_iff (schema : Schema) (root : Selection) :
    (∃ query, resolveSelection schema root = .ok query) ↔ acceptsSelection schema root = true
```

## Boundary

`Scan` contains the resolved permission expression syntactically; the proof does not
interpret PostgreSQL or show that a permission expresses its author's intended policy.
`queryReads` covers user selection, filtering, ordering and aggregate columns; join
keys and the appended permission conjunct are excluded. `documentValues` walks the
raw resolved plan in document order and inserts each scan's permission values after
its user filter; it is not an independent selection acceptance checker.

`resolveSelection` and `runSelection` have a complete success characterization for
the schema supplied to them. The choice of an error variant is outside that
characterization. The role-aware `resolve` and `run` remain outside this acceptance
proof: they additionally bind session variables, insert permissions and validate
all scans and user column references.

The missing step is an input-only success characterization of `guardQuery` followed
by `queryPermitted`, given successful resolution against `visibleSchema`. It must
include missing session variables and the exact syntactic permission dependencies
inside policy relationships. Successful raw selection and individually resolvable
role conditions are insufficient. For example, a permission on table `t` containing
a relationship predicate `u.id IS NOT NULL` can resolve while table `u` requires
`u.id = 1`; the final validator rejects this query with `InvalidPermission`.
The existing scan-condition theorem assumes successful final resolution, so it
cannot establish this converse. `guardQuery`, `resolve` and `run` stay in Survey.

## Write and returning boundaries

`runWrite` resolves a `WriteInput` to a `WritePlan` and emits that plan. The permission
statements concern these trees. `WriteCheck.abort` is distinct from `unchecked`;
`writeCheckToks` emits the integer cast that fails the PostgreSQL statement. SQL execution
and the mapping of this node to PostgreSQL's error behavior are tested, not proved.

`writeColumnAllowed` requires a declared scalar mapping and membership in either the
operation's allowed user columns or its preset columns. `resolveWrite_user_columns`
separately excludes preset columns from supplied inputs. `resolveWrite_tables` identifies
the exact role, target and operation in the permission catalog.
`resolveWrite_declared` connects that logical target to the physical SQL schema and table.

A conflict update is a separate `ConflictPlan`. `resolveWrite_conflict` proves that
`writePermitted` accepts its `conflictAsWrite` projection, whose operation is `update`;
this checks the update permission, update row condition, aborting update check and written
columns. `resolveConflict` checks user conflict columns against the update permission
and excludes presets. An inserted row uses the insert check; a conflicting updated row
uses the update check. PostgreSQL's `xmax` distinguishes these branches in `RETURNING`.

A mutation's top-level returning rows read the changed-row CTE. They are deliberately
excluded from `resolveWrite_returning_conditions`, which quantifies over `nodesScans`
and their predicate scans. Top-level projected columns remain checked by
`returningPermitted`. The same `scanPermitted_condition` lemma used by
`resolve_scan_conditions` proves the mutation statement for relationships and aggregates.
`resolve_scan_conditions` itself continues to describe query resolution.

SQL lowering redirects returning scans of the modified target to a post-operation
snapshot CTE. The validated plan retains its declared table names and role predicates;
`snapshotNode` and `snapshotPredicate` lower these sources for execution. This lowering,
including the primary-key self-join that keeps original tuple identifiers for updates and
deletes and the primary keys for conflicts, is outside the read-scan theorem.
Differential fixtures cover relationships that return to the modified table after insert,
update, delete and conflict update; concurrent-transaction tests cover update and delete.

`resolveWrite_validated`, `resolveWrite_secure` and `resolveWrite_conflict` expose the
final validators for the audit. `runWrite_accepts` restates success through a map over
resolution; it is not an independent input acceptance specification. `emitWrite_slots`
is an instance of `slots_numbered`. These supporting statements are not counted again.

## Mutation audit

The strict audit targets twenty-four functions: 154 mutations, 103 killed, zero holes,
18 equivalent on sampled inputs, zero untested, zero timed out and 33 stillborn.
`make survey` reports the functions outside the strict audit:

| Group | Functions | Mutations | Killed | Holes | Equivalent on sampled inputs | Untested | Timed out | Stillborn |
|---|---|---|---|---|---|---|---|---|
| SQL emission | 8 | 176 | 23 | 100 | 2 | 0 | 0 | 51 |
| Selection resolution | 2 | 136 | 25 | 3 | 2 | 0 | 52 | 54 |
| Permission resolution | 3 | 1 | 0 | 1 | 0 | 0 | 0 | 0 |
| Total | 13 | 313 | 48 | 104 | 4 | 0 | 52 | 105 |

The survey uses 1000 samples and a 30-second deadline per mutation. Equivalence is sampled output equality, not a proved equivalence.

The write insertion audit targets `applyWriteCondition`, `applyWriteCheck` and
`applyWritePermission`: ten mutations, all killed, with no hole, untested mutation,
timeout or stillborn mutation. Hand-written mutations replacing the check with
`unchecked` and replacing the update/delete condition with the user condition fail
`applyWriteCheck_exact` and `applyWriteCondition_exact` respectively, as well as the
corresponding projection of `applyWritePermission_conditions`. Manual failures are
reported separately from automatic kills.
