# Theorem statements

These statements are copied from the `model` block in `src/compiler.rs`. Definitions
and proof terms there are authoritative. The properties apply to all Lean schemas,
sessions and selections; a successful resolution is required where stated.

## Independent properties

Nine properties, expressed by fifteen boundary statements. `resolve_tables` is a
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

## Conditions restated next to their implementations

Fourteen statements; these do not supply an independent acceptance specification.

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

## Checks of individual values

None. Unit tests and fixtures are not counted as theorems.

## Boundary

`Scan` contains the resolved permission expression syntactically; the proof does not
interpret PostgreSQL or show that a permission expresses its author's intended policy.
`queryReads` covers user selection, filtering, ordering and aggregate columns; join
keys and the appended permission conjunct are excluded. `documentValues` walks the
raw resolved plan in document order and inserts each scan's permission values after
its user filter; it is not an independent selection acceptance checker.

The complete acceptance equivalence for `resolveSelection` and `resolve` is pending.
The existing predicates characterize individual checks, but no independent Bool
function characterizes all accepted selections. Neither the complete accepted set
nor the choice of every error is proved. These functions remain in `Survey`.

An independent acceptance checker still needs root lookup, mode-dependent selections,
argument restrictions, alias and depth bounds, visible schema lookup, session binding
and policy-dependency checks. It must return Bool without calling a resolver or
constructing `Query`, `Node` or `Scan`, followed by mutual success-equivalence proofs.

## Mutation audit

The strict audit targets fourteen functions: 101 mutations, 78 killed, zero holes,
16 equivalent on sampled inputs, zero untested, zero timed out and 7 stillborn.
`make survey` reports the functions outside the strict audit:

| Group | Functions | Mutations | Killed | Holes | Equivalent on sampled inputs | Untested | Timed out | Stillborn |
|---|---|---|---|---|---|---|---|---|
| SQL emission | 8 | 176 | 23 | 100 | 2 | 0 | 0 | 51 |
| Selection resolution | 9 | 178 | 5 | 50 | 17 | 0 | 26 | 80 |
| Permission resolution | 3 | 1 | 0 | 1 | 0 | 0 | 0 | 0 |
| Total | 20 | 355 | 28 | 151 | 19 | 0 | 26 | 131 |

The survey uses 1000 samples and a 30-second deadline per mutation. Equivalence is sampled output equality, not a proved equivalence.
