namespace model

inductive Value where
  | int (v : Int)
  | text (v : String)
  | bool (v : Bool)
  deriving DecidableEq, Repr

inductive Lex where
  | raw (text : String)
  | ident (name : String)
  | col (qualifier name : String)
  | nat (n : Nat)
  deriving DecidableEq, Repr

inductive Tok where
  | lex (l : Lex)
  | param (v : Value)
  deriving DecidableEq, Repr

inductive Piece where
  | text (s : String)
  | slot (index : Nat)
  deriving DecidableEq, Repr

structure Compiled where
  sql : String
  params : List Value
  deriving DecidableEq, Repr

def quoteChars : List Char → List Char
  | [] => ['"']
  | c :: cs => if c = '"' then '"' :: '"' :: quoteChars cs else c :: quoteChars cs

def unquoteChars : List Char → Option (List Char × List Char)
  | [] => none
  | [c] => if c = '"' then some ([], []) else none
  | c :: d :: cs =>
    if c = '"' then
      if d = '"' then (unquoteChars cs).map fun (name, rest) => ('"' :: name, rest)
      else some ([], d :: cs)
    else (unquoteChars (d :: cs)).map fun (name, rest) => (c :: name, rest)

def quote (name : String) : String := String.ofList ('"' :: quoteChars name.toList)

def lexText : Lex → String
  | .raw text => text
  | .ident name => quote name
  | .col qualifier name => quote qualifier ++ "." ++ quote name
  | .nat n => toString n

def lift (lexes : List Lex) : List Tok := lexes.map .lex

def tokValue : Tok → Option Value
  | .lex _ => none
  | .param v => some v

def eraseTok : Tok → Tok
  | .lex l => .lex l
  | .param _ => .param (.int 0)

def assignStep (state : List Piece × Nat) : Tok → List Piece × Nat
  | .lex l => (.text (lexText l) :: state.1, state.2)
  | .param _ => (.slot state.2 :: state.1, state.2 + 1)

def assign (toks : List Tok) : List Piece := (toks.foldl assignStep ([], 1)).1.reverse

def pieceSlot : Piece → Option Nat
  | .text _ => none
  | .slot index => some index

def pieceText : Piece → String
  | .text s => s
  | .slot index => "$" ++ toString index

def slots (toks : List Tok) : List Nat := (assign toks).filterMap pieceSlot

def render (toks : List Tok) : Compiled :=
  { sql := " ".intercalate ((assign toks).map pieceText), params := toks.filterMap tokValue }

theorem unquote_quote (name rest : List Char) (h : rest.head? ≠ some '"') :
    unquoteChars (quoteChars name ++ rest) = some (name, rest) := by
  induction name with
  | nil =>
    cases rest with
    | nil => simp [quoteChars, unquoteChars]
    | cons d rest =>
      have : d ≠ '"' := by simpa using h
      simp [quoteChars, unquoteChars, this]
  | cons c name ih =>
    by_cases hc : c = '"'
    · simp [quoteChars, unquoteChars, hc, ih]
    · cases hq : quoteChars name ++ rest with
      | nil => simp [hq, unquoteChars] at ih
      | cons d tail => simp [quoteChars, unquoteChars, hc, hq, ← ih]

theorem unquote_quoteChars (name : List Char) : unquoteChars (quoteChars name) = some (name, []) := by
  simpa using unquote_quote name [] (by simp)

theorem assign_foldl (toks : List Tok) (acc : List Piece) (next : Nat) :
    ((toks.foldl assignStep (acc, next)).1.reverse).filterMap pieceSlot
      = acc.reverse.filterMap pieceSlot ++ List.range' next (toks.filterMap tokValue).length := by
  induction toks generalizing acc next with
  | nil => simp
  | cons t toks ih =>
    cases t <;> simp [assignStep, ih, tokValue, pieceSlot, List.filterMap, List.range'_succ]

theorem slots_numbered (toks : List Tok) :
    slots toks = List.range' 1 (render toks).params.length := by
  have h := assign_foldl toks [] 1
  simpa [slots, assign, render] using h

theorem assign_erase_foldl (toks : List Tok) (state : List Piece × Nat) :
    (toks.map eraseTok).foldl assignStep state = toks.foldl assignStep state := by
  induction toks generalizing state with
  | nil => rfl
  | cons t toks ih => cases t <;> simp [eraseTok, assignStep, ih]

theorem render_erase (toks : List Tok) : (render (toks.map eraseTok)).sql = (render toks).sql := by
  simp [render, assign, assign_erase_foldl]

theorem lift_values (lexes : List Lex) : (lift lexes).filterMap tokValue = [] := by
  induction lexes with
  | nil => rfl
  | cons l lexes ih => simp [lift, tokValue, List.filterMap]

theorem lift_erase (lexes : List Lex) : (lift lexes).map eraseTok = lift lexes := by
  induction lexes with
  | nil => rfl
  | cons l lexes ih => simp [lift, eraseTok]

inductive Direction where
  | asc
  | desc
  deriving DecidableEq, Repr

inductive Kind where
  | object
  | array
  deriving DecidableEq, Repr

structure Scan where
  schema : String
  table : String
  conds : List (String × Value)
  order : List (String × Direction)
  limit : Option Nat
  deriving DecidableEq, Repr

inductive Node where
  | column (alias column : String)
  | relation (alias : String) (kind : Kind) (scan : Scan) (parent child : List String)
      (children : List Node)
  deriving Repr

structure Query where
  scan : Scan
  children : List Node
  deriving Repr

def tName (depth : Nat) : String := "t" ++ toString depth

def rName (depth : Nat) : String := "r" ++ toString depth

def cName (index : Nat) : String := "c" ++ toString index

def kName (index : Nat) : String := "k" ++ toString index

def commas : List (List Lex) → List Lex
  | [] => []
  | [x] => x
  | x :: y :: rest => x ++ [.raw ","] ++ commas (y :: rest)

def directionText : Direction → String
  | .asc => "ASC NULLS LAST"
  | .desc => "DESC NULLS FIRST"

def orderLex (qualifier : String) (order : List (String × Direction)) : List Lex :=
  if order.isEmpty then []
  else .raw "ORDER BY" :: commas (order.map fun o => [.col qualifier o.1, .raw (directionText o.2)])

def limitLex : Option Nat → List Lex
  | none => []
  | some n => [.raw "LIMIT", .nat n]

def condToks (cond : String × Value) : List Tok :=
  [.lex (.raw "AND"), .lex (.col "s" cond.1), .lex (.raw "="), .param cond.2]

def scanToks (scan : Scan) (link : List Lex) : List Tok :=
  lift ([.raw "SELECT \"s\".* FROM", .col scan.schema scan.table, .raw "AS \"s\" WHERE"] ++ link)
    ++ scan.conds.flatMap condToks
    ++ lift (orderLex "s" scan.order ++ limitLex scan.limit)

def isSet (set : Bool) (kind : Kind) (limit : Option Nat) : Bool :=
  set && kind == .array && limit.isNone

def fieldLex (depth index : Nat) (set : Bool) : Node → List Lex
  | .column alias column => [.col (tName depth) column, .raw "AS", .ident alias]
  | .relation alias kind scan _ _ _ =>
    if isSet set kind scan.limit then
      [.raw "coalesce(", .col (cName index) "j", .raw ", '[]') AS", .ident alias]
    else [.col (cName index) "j", .raw "AS", .ident alias]

def fieldsFrom (depth index : Nat) (set : Bool) : List Node → List (List Lex)
  | [] => []
  | n :: ns => fieldLex depth index set n :: fieldsFrom depth (index + 1) set ns

def rowLex (depth : Nat) (set : Bool) (children : List Node) : List Lex :=
  [.raw "row_to_json((SELECT \"%\" FROM (SELECT"] ++ commas (fieldsFrom depth 0 set children)
    ++ [.raw ") AS \"%\"))"]

def lateralLink (depth : Nat) (parent child : List String) : List Lex :=
  .raw "true" :: (parent.zip child).flatMap fun pair =>
    [.raw "AND", .col "s" pair.2, .raw "=", .col (tName depth) pair.1]

def setLink (depth : Nat) (parent child : List String) : List Lex :=
  [.raw "("] ++ commas (child.map fun c => [.col "s" c]) ++ [.raw ") IN (SELECT"]
    ++ commas (parent.map fun p => [.ident p]) ++ [.raw "FROM", .ident (rName depth), .raw ")"]

def keyLex (depth : Nat) (child : List String) : List Lex :=
  child.zipIdx.flatMap fun key => [.col (tName depth) key.1, .raw "AS", .ident (kName key.2), .raw ","]

def onLex (depth index : Nat) (parent : List String) : List Lex :=
  .raw "true" :: parent.zipIdx.flatMap fun key =>
    [.raw "AND", .col (cName index) (kName key.2), .raw "=", .col (tName depth) key.1]

def groupLex (depth : Nat) (child : List String) : List Lex :=
  commas (child.map fun c => [.col (tName depth) c])

def lateralScan (kind : Kind) (scan : Scan) : Scan :=
  match kind with
  | .object => { scan with order := [], limit := some 1 }
  | .array => scan

def lateralAgg (depth : Nat) (kind : Kind) (scan : Scan) (children : List Node) : List Lex :=
  match kind with
  | .object => rowLex depth false children
  | .array =>
    [.raw "coalesce(json_agg("] ++ rowLex depth false children ++ orderLex (tName depth) scan.order
      ++ [.raw "), '[]')"]

mutual
def joinToks (depth index : Nat) (set : Bool) : Node → List Tok
  | .column _ _ => []
  | .relation _ kind scan parent child children =>
    if isSet set kind scan.limit then
      lift [.raw "LEFT JOIN (WITH", .ident (rName (depth + 1)), .raw "AS ("]
        ++ scanToks scan (setLink depth parent child)
        ++ lift ([.raw ") SELECT"] ++ keyLex (depth + 1) child ++ [.raw "json_agg("]
          ++ rowLex (depth + 1) true children ++ orderLex (tName (depth + 1)) scan.order
          ++ [.raw ") AS \"j\" FROM", .ident (rName (depth + 1)), .raw "AS", .ident (tName (depth + 1))])
        ++ joinsToks (depth + 1) 0 true children
        ++ lift ([.raw "GROUP BY"] ++ groupLex (depth + 1) child
          ++ [.raw ") AS", .ident (cName index), .raw "ON"] ++ onLex depth index parent)
    else
      lift ([.raw "LEFT JOIN LATERAL (SELECT"] ++ lateralAgg (depth + 1) kind scan children
          ++ [.raw "AS \"j\" FROM ("])
        ++ scanToks (lateralScan kind scan) (lateralLink depth parent child)
        ++ lift [.raw ") AS", .ident (tName (depth + 1))]
        ++ joinsToks (depth + 1) 0 false children
        ++ lift [.raw ") AS", .ident (cName index), .raw "ON true"]
def joinsToks (depth index : Nat) (set : Bool) : List Node → List Tok
  | [] => []
  | n :: ns => joinToks depth index set n ++ joinsToks depth (index + 1) set ns
end

def emit (query : Query) : List Tok :=
  lift [.raw "WITH", .ident (rName 0), .raw "AS ("] ++ scanToks query.scan [.raw "true"]
    ++ lift ([.raw ") SELECT coalesce(json_agg("] ++ rowLex 0 true query.children
      ++ orderLex (tName 0) query.scan.order
      ++ [.raw "), '[]') FROM", .ident (rName 0), .raw "AS", .ident (tName 0)])
    ++ joinsToks 0 0 true query.children

def compile (query : Query) : Compiled := render (emit query)

def placeholders (query : Query) : List Nat := slots (emit query)

def scanValues (scan : Scan) : List Value := scan.conds.map (·.2)

mutual
def nodeValues : Node → List Value
  | .column _ _ => []
  | .relation _ _ scan _ _ children => scanValues scan ++ nodesValues children
def nodesValues : List Node → List Value
  | [] => []
  | n :: ns => nodeValues n ++ nodesValues ns
end

def queryValues (query : Query) : List Value := scanValues query.scan ++ nodesValues query.children

def eraseScan (scan : Scan) : Scan := { scan with conds := scan.conds.map fun c => (c.1, .int 0) }

mutual
def eraseNode : Node → Node
  | .column alias column => .column alias column
  | .relation alias kind scan parent child children =>
    .relation alias kind (eraseScan scan) parent child (eraseNodes children)
def eraseNodes : List Node → List Node
  | [] => []
  | n :: ns => eraseNode n :: eraseNodes ns
end

def eraseQuery (query : Query) : Query :=
  { scan := eraseScan query.scan, children := eraseNodes query.children }

theorem conds_values (conds : List (String × Value)) :
    (conds.flatMap condToks).filterMap tokValue = conds.map (·.2) := by
  induction conds with
  | nil => rfl
  | cons c conds ih => simp [condToks, tokValue, List.filterMap, ih]

theorem conds_erase (conds : List (String × Value)) :
    (conds.flatMap condToks).map eraseTok = (conds.map fun c => (c.1, Value.int 0)).flatMap condToks := by
  induction conds with
  | nil => rfl
  | cons c conds ih => simp [condToks, eraseTok, ih]

theorem scanToks_values (scan : Scan) (link : List Lex) :
    (scanToks scan link).filterMap tokValue = scanValues scan := by
  simp [scanToks, scanValues, lift_values, conds_values]

theorem scanToks_erase (scan : Scan) (link : List Lex) :
    (scanToks scan link).map eraseTok = scanToks (eraseScan scan) link := by
  simp [scanToks, eraseScan, lift_erase, conds_erase]

theorem lateralScan_values (kind : Kind) (scan : Scan) :
    scanValues (lateralScan kind scan) = scanValues scan := by
  cases kind <;> rfl

theorem lateralScan_erase (kind : Kind) (scan : Scan) :
    eraseScan (lateralScan kind scan) = lateralScan kind (eraseScan scan) := by
  cases kind <;> rfl

mutual
theorem joinToks_values (depth index : Nat) (set : Bool) (n : Node) :
    (joinToks depth index set n).filterMap tokValue = nodeValues n := by
  cases n with
  | column alias column => simp [joinToks, nodeValues]
  | relation alias kind scan parent child children =>
    simp only [joinToks, nodeValues]
    split <;>
      simp [lift_values, scanToks_values, lateralScan_values,
        joinsToks_values (depth + 1) 0 _ children]
theorem joinsToks_values (depth index : Nat) (set : Bool) (ns : List Node) :
    (joinsToks depth index set ns).filterMap tokValue = nodesValues ns := by
  cases ns with
  | nil => simp [joinsToks, nodesValues]
  | cons n ns =>
    simp [joinsToks, nodesValues, joinToks_values depth index set n,
      joinsToks_values depth (index + 1) set ns]
end

theorem fieldLex_erase (depth index : Nat) (set : Bool) (n : Node) :
    fieldLex depth index set (eraseNode n) = fieldLex depth index set n := by
  cases n <;> simp [eraseNode, fieldLex, eraseScan]

theorem fieldsFrom_erase (depth index : Nat) (set : Bool) (ns : List Node) :
    fieldsFrom depth index set (eraseNodes ns) = fieldsFrom depth index set ns := by
  induction ns generalizing index with
  | nil => simp [eraseNodes, fieldsFrom]
  | cons n ns ih => simp [eraseNodes, fieldsFrom, fieldLex_erase, ih]

theorem rowLex_erase (depth : Nat) (set : Bool) (ns : List Node) :
    rowLex depth set (eraseNodes ns) = rowLex depth set ns := by
  simp [rowLex, fieldsFrom_erase]

theorem lateralAgg_erase (depth : Nat) (kind : Kind) (scan : Scan) (ns : List Node) :
    lateralAgg depth kind (eraseScan scan) (eraseNodes ns) = lateralAgg depth kind scan ns := by
  cases kind <;> simp [lateralAgg, rowLex_erase, eraseScan]

mutual
theorem joinToks_erase (depth index : Nat) (set : Bool) (n : Node) :
    (joinToks depth index set n).map eraseTok = joinToks depth index set (eraseNode n) := by
  cases n with
  | column alias column => simp [joinToks, eraseNode]
  | relation alias kind scan parent child children =>
    simp only [joinToks, eraseNode]
    have hlimit : (eraseScan scan).limit = scan.limit := rfl
    have horder : (eraseScan scan).order = scan.order := rfl
    rw [hlimit, horder]
    split <;>
      simp [lift_erase, scanToks_erase, lateralScan_erase, rowLex_erase, lateralAgg_erase,
        joinsToks_erase (depth + 1) 0 _ children]
theorem joinsToks_erase (depth index : Nat) (set : Bool) (ns : List Node) :
    (joinsToks depth index set ns).map eraseTok = joinsToks depth index set (eraseNodes ns) := by
  cases ns with
  | nil => simp [joinsToks, eraseNodes]
  | cons n ns =>
    simp [joinsToks, eraseNodes, joinToks_erase depth index set n,
      joinsToks_erase depth (index + 1) set ns]
end

theorem emit_values (query : Query) : (emit query).filterMap tokValue = queryValues query := by
  simp [emit, queryValues, lift_values, scanToks_values, joinsToks_values]

theorem emit_erase (query : Query) : (emit query).map eraseTok = emit (eraseQuery query) := by
  have horder : (eraseScan query.scan).order = query.scan.order := rfl
  simp [emit, eraseQuery, lift_erase, scanToks_erase, joinsToks_erase, rowLex_erase, horder]

theorem compile_params (query : Query) : (compile query).params = queryValues query := by
  simp [compile, render, emit_values]

theorem compile_erase (query : Query) : (compile (eraseQuery query)).sql = (compile query).sql := by
  simp [compile, ← emit_erase, render_erase]

theorem placeholders_numbered (query : Query) :
    placeholders query = List.range' 1 (queryValues query).length := by
  have h := compile_params query
  simp only [compile] at h
  simp [placeholders, slots_numbered, h]

theorem lateralScan_object (scan : Scan) :
    lateralScan .object scan = { scan with order := [], limit := some 1 } := rfl

theorem lateralScan_array (scan : Scan) : lateralScan .array scan = scan := rfl

structure Column where
  field : String
  column : String
  deriving DecidableEq, Repr

structure Relation where
  field : String
  kind : Kind
  target : String
  parent : List String
  child : List String
  deriving DecidableEq, Repr

structure Table where
  name : String
  schema : String
  table : String
  columns : List Column
  relations : List Relation
  deriving DecidableEq, Repr

structure Schema where
  tables : List Table
  deriving DecidableEq, Repr

structure Cond where
  field : String
  value : Value
  deriving DecidableEq, Repr

structure Order where
  field : String
  direction : Direction
  deriving DecidableEq, Repr

structure Selection where
  alias : String
  name : String
  conds : List Cond
  order : List Order
  limit : Option Nat
  children : List Selection
  deriving Repr

inductive Error where
  | unknownType (name : String)
  | unknownField (type field : String)
  | columnArguments (type field : String)
  | objectArguments (type field : String)
  | invalidRelation (type field : String)
  | invalidSelection (type : String)
  | tooDeep
  deriving DecidableEq, Repr

def maxDepth : Nat := 32

def maxFields : Nat := 1000

def rowAlias : String := "%"

def findColumn (table : Table) (field : String) : Option String :=
  (table.columns.find? (·.field = field)).map (·.column)

def findRelation (table : Table) (field : String) : Option Relation :=
  table.relations.find? (·.field = field)

def findTable (schema : Schema) (name : String) : Option Table :=
  schema.tables.find? (·.name = name)

def resolveConds (table : Table) : List Cond → Except Error (List (String × Value))
  | [] => .ok []
  | c :: cs =>
    match findColumn table c.field with
    | none => .error (.unknownField table.name c.field)
    | some column =>
      match resolveConds table cs with
      | .error e => .error e
      | .ok rest => .ok ((column, c.value) :: rest)

def resolveOrder (table : Table) : List Order → Except Error (List (String × Direction))
  | [] => .ok []
  | o :: os =>
    match findColumn table o.field with
    | none => .error (.unknownField table.name o.field)
    | some column =>
      match resolveOrder table os with
      | .error e => .error e
      | .ok rest => .ok ((column, o.direction) :: rest)

def resolveScan (table : Table) (conds : List Cond) (order : List Order) (limit : Option Nat) :
    Except Error Scan :=
  match resolveConds table conds with
  | .error e => .error e
  | .ok conds =>
    match resolveOrder table order with
    | .error e => .error e
    | .ok order => .ok { schema := table.schema, table := table.table, conds, order, limit }

def aliasValid (alias : String) : Bool :=
  alias ≠ "" && alias ≠ rowAlias && alias.utf8ByteSize ≤ 63

def distinct : List String → Bool
  | [] => true
  | a :: rest => !rest.contains a && distinct rest

def selectionValid (aliases : List String) : Bool :=
  !aliases.isEmpty && aliases.length ≤ maxFields && aliases.all aliasValid && distinct aliases

def relationValid (relation : Relation) : Bool :=
  !relation.parent.isEmpty && relation.parent.length == relation.child.length

def plain (selection : Selection) : Bool :=
  selection.conds.isEmpty && selection.order.isEmpty && selection.limit.isNone

mutual
def resolveNode (schema : Schema) (table : Table) (depth : Nat) : Selection → Except Error Node
  | ⟨alias, name, conds, order, limit, children⟩ =>
    match findColumn table name with
    | some column =>
      if conds.isEmpty && order.isEmpty && limit.isNone && children.isEmpty then
        .ok (.column alias column)
      else .error (.columnArguments table.name name)
    | none =>
      match findRelation table name with
      | none => .error (.unknownField table.name name)
      | some relation =>
        match findTable schema relation.target with
        | none => .error (.unknownType relation.target)
        | some target =>
          if maxDepth ≤ depth then .error .tooDeep
          else if relation.kind = .object && !(conds.isEmpty && order.isEmpty && limit.isNone) then
            .error (.objectArguments table.name name)
          else if !relationValid relation then .error (.invalidRelation table.name name)
          else if !selectionValid (children.map (·.alias)) then .error (.invalidSelection target.name)
          else
            match resolveScan target conds order limit with
            | .error e => .error e
            | .ok scan =>
              match resolveNodes schema target (depth + 1) children with
              | .error e => .error e
              | .ok nodes => .ok (.relation alias relation.kind scan relation.parent relation.child nodes)
def resolveNodes (schema : Schema) (table : Table) (depth : Nat) :
    List Selection → Except Error (List Node)
  | [] => .ok []
  | s :: ss =>
    match resolveNode schema table depth s with
    | .error e => .error e
    | .ok node =>
      match resolveNodes schema table depth ss with
      | .error e => .error e
      | .ok nodes => .ok (node :: nodes)
end

def resolve (schema : Schema) (root : Selection) : Except Error Query :=
  match findTable schema root.name with
  | none => .error (.unknownType root.name)
  | some table =>
    if !selectionValid (root.children.map (·.alias)) then .error (.invalidSelection table.name)
    else
      match resolveScan table root.conds root.order root.limit with
      | .error e => .error e
      | .ok scan =>
        match resolveNodes schema table 0 root.children with
        | .error e => .error e
        | .ok children => .ok { scan, children }

def run (schema : Schema) (root : Selection) : Except Error Compiled :=
  match resolve schema root with
  | .error e => .error e
  | .ok query => .ok (compile query)

mutual
def selectionValues : Selection → List Value
  | ⟨_, _, conds, _, _, children⟩ => conds.map (·.value) ++ selectionsValues children
def selectionsValues : List Selection → List Value
  | [] => []
  | s :: ss => selectionValues s ++ selectionsValues ss
end

theorem resolveConds_values (table : Table) (conds : List Cond) (out : List (String × Value))
    (h : resolveConds table conds = .ok out) : out.map (·.2) = conds.map (·.value) := by
  induction conds generalizing out with
  | nil => simp [resolveConds] at h; simp [← h]
  | cons c cs ih =>
    simp only [resolveConds] at h
    split at h
    · cases h
    · split at h
      · cases h
      · rename_i rest hrest
        cases h
        simp [ih rest hrest]

theorem resolveScan_values (table : Table) (conds : List Cond) (order : List Order)
    (limit : Option Nat) (scan : Scan) (h : resolveScan table conds order limit = .ok scan) :
    scanValues scan = conds.map (·.value) := by
  simp only [resolveScan] at h
  split at h
  · cases h
  · rename_i out hout
    split at h
    · cases h
    · cases h
      simpa [scanValues] using resolveConds_values table conds out hout

mutual
theorem resolveNode_values (schema : Schema) (table : Table) (depth : Nat) (s : Selection)
    (n : Node) (h : resolveNode schema table depth s = .ok n) :
    nodeValues n = selectionValues s := by
  obtain ⟨alias, name, conds, order, limit, children⟩ := s
  simp only [resolveNode] at h
  split at h
  · split at h
    · rename_i hplain
      cases h
      simp at hplain
      simp [nodeValues, selectionValues, selectionsValues, hplain]
    · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · split at h
        · cases h
        · split at h
          · cases h
          · split at h
            · cases h
            · split at h
              · cases h
              · split at h
                · cases h
                · rename_i scan hscan
                  split at h
                  · cases h
                  · rename_i nodes hnodes
                    cases h
                    simp [nodeValues, selectionValues, resolveScan_values _ _ _ _ _ hscan,
                      resolveNodes_values _ _ _ _ _ hnodes]
theorem resolveNodes_values (schema : Schema) (table : Table) (depth : Nat) (ss : List Selection)
    (ns : List Node) (h : resolveNodes schema table depth ss = .ok ns) :
    nodesValues ns = selectionsValues ss := by
  cases ss with
  | nil => simp [resolveNodes] at h; simp [h, nodesValues, selectionsValues]
  | cons s ss =>
    simp only [resolveNodes] at h
    split at h
    · cases h
    · rename_i node hnode
      split at h
      · cases h
      · rename_i nodes hnodes
        cases h
        simp [nodesValues, selectionsValues, resolveNode_values _ _ _ _ _ hnode,
          resolveNodes_values _ _ _ _ _ hnodes]
end

theorem resolve_values (schema : Schema) (root : Selection) :
    match resolve schema root with
    | .ok query => queryValues query = selectionValues root
    | .error _ => True := by
  split
  · rename_i query h
    obtain ⟨alias, name, conds, order, limit, children⟩ := root
    simp only [resolve] at h
    split at h
    · cases h
    · split at h
      · cases h
      · split at h
        · cases h
        · rename_i scan hscan
          split at h
          · cases h
          · rename_i nodes hnodes
            cases h
            simp [queryValues, selectionValues, resolveScan_values _ _ _ _ _ hscan,
              resolveNodes_values _ _ _ _ _ hnodes]
  · trivial

theorem run_params (schema : Schema) (root : Selection) :
    match run schema root with
    | .ok compiled => compiled.params = selectionValues root
    | .error _ => True := by
  have h := resolve_values schema root
  simp only [run]
  split at h
  · rename_i query hq
    simp [hq, compile_params, h]
  · rename_i e hq
    simp [hq]

def declared (schema : Schema) (scan : Scan) : Bool :=
  schema.tables.any fun t => t.schema = scan.schema && t.table = scan.table

mutual
def nodeScans : Node → List Scan
  | .column _ _ => []
  | .relation _ _ scan _ _ children => scan :: nodesScans children
def nodesScans : List Node → List Scan
  | [] => []
  | n :: ns => nodeScans n ++ nodesScans ns
end

def queryScans (query : Query) : List Scan := query.scan :: nodesScans query.children

theorem findTable_declared (schema : Schema) (name : String) (table : Table)
    (conds : List Cond) (order : List Order) (limit : Option Nat) (scan : Scan)
    (hfind : findTable schema name = some table)
    (hscan : resolveScan table conds order limit = .ok scan) : declared schema scan = true := by
  have hmem : table ∈ schema.tables := List.mem_of_find?_eq_some hfind
  simp only [resolveScan] at hscan
  split at hscan
  · cases hscan
  · split at hscan
    · cases hscan
    · cases hscan
      simp only [declared, List.any_eq_true]
      exact ⟨table, hmem, by simp⟩

mutual
theorem resolveNode_declared (schema : Schema) (table : Table) (depth : Nat) (s : Selection)
    (n : Node) (h : resolveNode schema table depth s = .ok n) :
    (nodeScans n).all (declared schema) = true := by
  obtain ⟨alias, name, conds, order, limit, children⟩ := s
  simp only [resolveNode] at h
  split at h
  · split at h
    · cases h
      simp [nodeScans]
    · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · rename_i target htarget
        split at h
        · cases h
        · split at h
          · cases h
          · split at h
            · cases h
            · split at h
              · cases h
              · split at h
                · cases h
                · rename_i scan hscan
                  split at h
                  · cases h
                  · rename_i nodes hnodes
                    cases h
                    simp only [nodeScans, List.all_cons, Bool.and_eq_true]
                    exact ⟨findTable_declared _ _ _ _ _ _ _ htarget hscan,
                      resolveNodes_declared _ _ _ _ _ hnodes⟩
theorem resolveNodes_declared (schema : Schema) (table : Table) (depth : Nat)
    (ss : List Selection) (ns : List Node) (h : resolveNodes schema table depth ss = .ok ns) :
    (nodesScans ns).all (declared schema) = true := by
  cases ss with
  | nil => simp [resolveNodes] at h; simp [h, nodesScans]
  | cons s ss =>
    simp only [resolveNodes] at h
    split at h
    · cases h
    · rename_i node hnode
      split at h
      · cases h
      · rename_i nodes hnodes
        cases h
        simp only [nodesScans, List.all_append, Bool.and_eq_true]
        exact ⟨resolveNode_declared _ _ _ _ _ hnode, resolveNodes_declared _ _ _ _ _ hnodes⟩
end

theorem resolve_declared (schema : Schema) (root : Selection) :
    match resolve schema root with
    | .ok query => (queryScans query).all (declared schema) = true
    | .error _ => True := by
  split
  · rename_i query h
    simp only [resolve] at h
    split at h
    · cases h
    · rename_i table htable
      split at h
      · cases h
      · split at h
        · cases h
        · rename_i scan hscan
          split at h
          · cases h
          · rename_i nodes hnodes
            cases h
            simp only [queryScans, List.all_cons, Bool.and_eq_true]
            exact ⟨findTable_declared _ _ _ _ _ _ _ htable hscan,
              resolveNodes_declared _ _ _ _ _ hnodes⟩
  · trivial

theorem distinct_iff (aliases : List String) : distinct aliases = true ↔ aliases.Nodup := by
  induction aliases with
  | nil => simp [distinct]
  | cons a rest ih => simp [distinct, ih]

theorem aliasValid_iff (alias : String) :
    aliasValid alias = true ↔ alias ≠ "" ∧ alias ≠ rowAlias ∧ alias.utf8ByteSize ≤ 63 := by
  simp [aliasValid, and_assoc]

theorem selectionValid_iff (aliases : List String) :
    selectionValid aliases = true ↔
      aliases ≠ [] ∧ aliases.length ≤ maxFields ∧ (∀ a, a ∈ aliases → aliasValid a = true)
        ∧ aliases.Nodup := by
  simp [selectionValid, distinct_iff, and_assoc]

theorem relationValid_iff (relation : Relation) :
    relationValid relation = true ↔
      relation.parent ≠ [] ∧ relation.parent.length = relation.child.length := by
  simp [relationValid]

end model
