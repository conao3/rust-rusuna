namespace model

inductive Value where
  | int (v : Int)
  | text (v : String)
  | bool (v : Bool)
  | float (bits : UInt64)
  | null
  deriving DecidableEq, Repr

inductive Lex where
  | raw (text : String)
  | ident (name : String)
  | col (qualifier name : String)
  | nat (n : Nat)
  | string (text : String)
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

def quoteStringChars : List Char → List Char
  | [] => ['\'']
  | c :: cs => if c = '\'' then '\'' :: '\'' :: quoteStringChars cs else c :: quoteStringChars cs

def unquoteStringChars : List Char → Option (List Char × List Char)
  | [] => none
  | [c] => if c = '\'' then some ([], []) else none
  | c :: d :: cs =>
    if c = '\'' then
      if d = '\'' then (unquoteStringChars cs).map fun (name, rest) => ('\'' :: name, rest)
      else some ([], d :: cs)
    else (unquoteStringChars (d :: cs)).map fun (name, rest) => (c :: name, rest)

def quoteString (name : String) : String := String.ofList ('\'' :: quoteStringChars name.toList)

def lexText : Lex → String
  | .raw text => text
  | .ident name => quote name
  | .col qualifier name => quote qualifier ++ "." ++ quote name
  | .nat n => toString n
  | .string text => quoteString text

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

theorem unquote_quoteString (name rest : List Char) (h : rest.head? ≠ some '\'') :
    unquoteStringChars (quoteStringChars name ++ rest) = some (name, rest) := by
  induction name with
  | nil =>
    cases rest with
    | nil => simp [quoteStringChars, unquoteStringChars]
    | cons d rest =>
      have : d ≠ '\'' := by simpa using h
      simp [quoteStringChars, unquoteStringChars, this]
  | cons c name ih =>
    by_cases hc : c = '\''
    · simp [quoteStringChars, unquoteStringChars, hc, ih]
    · cases hq : quoteStringChars name ++ rest with
      | nil => simp [hq, unquoteStringChars] at ih
      | cons d tail => simp [quoteStringChars, unquoteStringChars, hc, hq, ← ih]

theorem unquote_quoteStringChars (name : List Char) : unquoteStringChars (quoteStringChars name) = some (name, []) := by
  simpa using unquote_quoteString name [] (by simp)

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
  | ascNullsFirst
  | ascNullsLast
  | descNullsFirst
  | descNullsLast
  deriving DecidableEq, Repr

inductive Kind where
  | object
  | array
  | aggregate
  deriving DecidableEq, Repr

inductive Comparison where
  | eq | neq | gt | gte | lt | lte | like | nlike | ilike | nilike
  deriving DecidableEq, Repr

structure Reference where
  schema : String
  table : String
  parent : List String
  child : List String
  deriving DecidableEq, Repr

inductive Expression (R V : Type) where
  | compare (field : String) (op : Comparison) (value : V)
  | member (field : String) (negated : Bool) (values : List V)
  | isNull (field : String) (value : Bool)
  | and (children : List (Expression R V))
  | or (children : List (Expression R V))
  | not (child : Expression R V)
  | relation (reference : R) (child : Expression R V)
  deriving Repr

mutual
def expressionValues {R V : Type} : Expression R V → List V
  | .compare _ _ v => [v]
  | .member _ _ vs => vs
  | .isNull _ _ => []
  | .and es | .or es => expressionsValues es
  | .not e | .relation _ e => expressionValues e
def expressionsValues {R V : Type} : List (Expression R V) → List V
  | [] => []
  | e :: es => expressionValues e ++ expressionsValues es
end

mutual
def eraseExpression {R : Type} : Expression R Value → Expression R Value
  | .compare f op _ => .compare f op (.int 0)
  | .member f neg vs => .member f neg (vs.map fun _ => .int 0)
  | .isNull f b => .isNull f b
  | .and es => .and (eraseExpressions es)
  | .or es => .or (eraseExpressions es)
  | .not e => .not (eraseExpression e)
  | .relation r e => .relation r (eraseExpression e)
def eraseExpressions {R : Type} : List (Expression R Value) → List (Expression R Value)
  | [] => []
  | e :: es => eraseExpression e :: eraseExpressions es
end

mutual
inductive Predicate where
  | compare (field : String) (op : Comparison) (value : Value)
  | member (field : String) (negated : Bool) (values : List Value)
  | isNull (field : String) (value : Bool)
  | and (children : List Predicate)
  | or (children : List Predicate)
  | not (child : Predicate)
  | exists (scan : Scan) (parent child : List String)
  deriving Repr
inductive Scan where
  | mk (name schema table : String) (conds : Predicate) (order : List (String × Direction))
      (limit offset : Option Nat)
  deriving Repr
end

def Scan.name : Scan → String | .mk name _ _ _ _ _ _ => name
def Scan.schema : Scan → String | .mk _ schema _ _ _ _ _ => schema
def Scan.table : Scan → String | .mk _ _ table _ _ _ _ => table
def Scan.conds : Scan → Predicate | .mk _ _ _ conds _ _ _ => conds
def Scan.order : Scan → List (String × Direction) | .mk _ _ _ _ order _ _ => order
def Scan.limit : Scan → Option Nat | .mk _ _ _ _ _ limit _ => limit
def Scan.offset : Scan → Option Nat | .mk _ _ _ _ _ _ offset => offset

mutual
def predicateValues : Predicate → List Value
  | .compare _ _ v => [v]
  | .member _ _ vs => vs
  | .isNull _ _ => []
  | .and es | .or es => predicatesValues es
  | .not e => predicateValues e
  | .exists (.mk _ _ _ conds _ _ _) _ _ => predicateValues conds
def predicatesValues : List Predicate → List Value
  | [] => []
  | e :: es => predicateValues e ++ predicatesValues es
end

mutual
def erasePredicate : Predicate → Predicate
  | .compare f op _ => .compare f op (.int 0)
  | .member f neg vs => .member f neg (vs.map fun _ => .int 0)
  | .isNull f b => .isNull f b
  | .and es => .and (erasePredicates es)
  | .or es => .or (erasePredicates es)
  | .not e => .not (erasePredicate e)
  | .exists (.mk name schema table conds order limit offset) parent child =>
    .exists (.mk name schema table (erasePredicate conds) order limit offset) parent child
def erasePredicates : List Predicate → List Predicate
  | [] => []
  | e :: es => erasePredicate e :: erasePredicates es
end

def comparisonText : Comparison → String
  | .eq => "=" | .neq => "<>" | .gt => ">" | .gte => ">=" | .lt => "<" | .lte => "<="
  | .like => "LIKE" | .nlike => "NOT LIKE" | .ilike => "ILIKE" | .nilike => "NOT ILIKE"

def paramToks : List Value → List Tok
  | [] => []
  | v :: vs => .param v :: vs.flatMap (fun x => [.lex (.raw ","), .param x])

mutual
def expressionToks (depth : Nat) (qualifier : String) : Predicate → List Tok
  | .compare field op v => lift [.col qualifier field, .raw (comparisonText op)] ++ [.param v]
  | .member field neg vs =>
    if vs.isEmpty then lift [.raw (if neg then "true" else "false")]
    else lift [.col qualifier field, .raw (if neg then "NOT IN (" else "IN (")]
      ++ paramToks vs ++ lift [.raw ")"]
  | .isNull field b => lift [.col qualifier field, .raw (if b then "IS NULL" else "IS NOT NULL")]
  | .and es => lift [.raw "( true"] ++ expressionsToks depth qualifier "AND" es ++ lift [.raw ")"]
  | .or es => lift [.raw "( false"] ++ expressionsToks depth qualifier "OR" es ++ lift [.raw ")"]
  | .not e => lift [.raw "NOT ("] ++ expressionToks depth qualifier e ++ lift [.raw ")"]
  | .exists (.mk _ schema table e _ _ _) parent child =>
    let alias := "e" ++ toString depth
    lift ([.raw "EXISTS (SELECT 1 FROM", .col schema table, .raw "AS", .ident alias,
      .raw "WHERE true"] ++ (parent.zip child).flatMap (fun pair =>
        [.raw "AND", .col alias pair.2, .raw "=", .col qualifier pair.1]) ++ [.raw "AND ("])
      ++ expressionToks (depth + 1) alias e ++ lift [.raw "))"]
def expressionsToks (depth : Nat) (qualifier op : String) : List (Predicate) → List Tok
  | [] => []
  | e :: es => lift [.raw op, .raw "("] ++ expressionToks depth qualifier e ++ lift [.raw ")"]
      ++ expressionsToks depth qualifier op es
end

theorem paramToks_values (vs : List Value) : (paramToks vs).filterMap tokValue = vs := by
  cases vs <;> simp [paramToks, tokValue, List.filterMap_flatMap, List.filterMap]

theorem paramToks_erase (vs : List Value) :
    (paramToks vs).map eraseTok = paramToks (vs.map fun _ => .int 0) := by
  cases vs <;> simp [paramToks, eraseTok, List.map_flatMap, List.flatMap_map]

mutual
theorem expressionToks_values (depth : Nat) (q : String) (e : Predicate) :
    (expressionToks depth q e).filterMap tokValue = predicateValues e := by
  cases e
  case «exists» scan parent child =>
    cases scan
    simp [expressionToks, predicateValues, lift_values, expressionToks_values]
  all_goals simp only [expressionToks, predicateValues]
  all_goals try split
  all_goals simp_all [lift_values, paramToks_values, tokValue, List.filterMap, expressionToks_values, expressionsToks_values]
theorem expressionsToks_values (depth : Nat) (q op : String) (es : List (Predicate)) :
    (expressionsToks depth q op es).filterMap tokValue = predicatesValues es := by
  cases es <;> simp [expressionsToks, predicatesValues, lift_values, expressionToks_values, expressionsToks_values]
end

mutual
theorem expressionToks_erase (depth : Nat) (q : String) (e : Predicate) :
    (expressionToks depth q e).map eraseTok = expressionToks depth q (erasePredicate e) := by
  cases e
  case «exists» scan parent child =>
    cases scan
    simp [expressionToks, erasePredicate, lift_erase, expressionToks_erase]
  all_goals simp only [expressionToks, erasePredicate]
  all_goals try split
  all_goals simp_all [lift_erase, paramToks_erase, eraseTok, expressionToks_erase, expressionsToks_erase]
theorem expressionsToks_erase (depth : Nat) (q op : String) (es : List (Predicate)) :
    (expressionsToks depth q op es).map eraseTok = expressionsToks depth q op (erasePredicates es) := by
  cases es <;> simp [expressionsToks, erasePredicates, lift_erase, expressionToks_erase, expressionsToks_erase]
end

inductive AggregateOp where
  | sum | avg | min | max
  deriving DecidableEq, Repr

def aggregateText : AggregateOp → String
  | .sum => "sum(" | .avg => "avg(" | .min => "min(" | .max => "max("

inductive Leaf where
  | column (name : String)
  | typename (name : String)
  | count
  | metric (op : AggregateOp) (name : String)
  deriving DecidableEq, Repr

def leafLex (qualifier : String) : Leaf → List Lex
  | .column name => [.col qualifier name]
  | .typename name => [.string name]
  | .count => [.raw "count(", .ident qualifier, .raw ".*)"]
  | .metric op name => [.raw (aggregateText op), .col qualifier name, .raw ")"]

inductive Node where
  | group (alias : String) (children : List Node)
  | nodes (alias : String) (order : List (String × Direction)) (children : List Node)
  | column (alias : String) (column : Leaf)
  | relation (alias : String) (kind : Kind) (scan : Scan) (parent child : List String)
      (children : List Node)
  deriving Repr

structure Query where
  kind : Kind
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
  | .ascNullsFirst => "ASC NULLS FIRST"
  | .ascNullsLast => "ASC NULLS LAST"
  | .descNullsFirst => "DESC NULLS FIRST"
  | .descNullsLast => "DESC NULLS LAST"

def orderLex (qualifier : String) (order : List (String × Direction)) : List Lex :=
  if order.isEmpty then []
  else .raw "ORDER BY" :: commas (order.map fun o => [.col qualifier o.1, .raw (directionText o.2)])

def limitLex : Option Nat → List Lex
  | none => []
  | some n => [.raw "LIMIT", .nat n]

def offsetLex : Option Nat → List Lex
  | none => []
  | some n => [.raw "OFFSET", .nat n]

def scanToks (scan : Scan) (link : List Lex) : List Tok :=
  lift ([.raw "SELECT \"s\".* FROM", .col scan.schema scan.table, .raw "AS \"s\" WHERE"] ++ link)
    ++ lift [.raw "AND ("] ++ expressionToks 0 "s" scan.conds ++ lift [.raw ")"]
    ++ lift (orderLex "s" scan.order ++ limitLex scan.limit ++ offsetLex scan.offset)

def isSet (set : Bool) (kind : Kind) (limit offset : Option Nat) : Bool :=
  set && kind != .object && limit.isNone && offset.isNone

def emptyLeaf : Leaf → List Lex
  | .count => [.nat 0]
  | .typename name => [.string name]
  | _ => [.raw "null"]

mutual
def emptyField : Node → List Lex
  | .column alias leaf => emptyLeaf leaf ++ [.raw "AS", .ident alias]
  | .group alias children => emptyRow children ++ [.raw "AS", .ident alias]
  | .nodes alias _ _ => [.raw "'[]'::json AS", .ident alias]
  | .relation alias _ _ _ _ _ => [.raw "null AS", .ident alias]
def emptyFields : List Node → List (List Lex)
  | [] => []
  | n :: ns => emptyField n :: emptyFields ns
def emptyRow (children : List Node) : List Lex :=
  [.raw "row_to_json((SELECT \"%\" FROM (SELECT"] ++ commas (emptyFields children)
    ++ [.raw ") AS \"%\"))"]
end

mutual
def fieldLex (depth index : Nat) (set : Bool) : Node → List Lex
  | .column alias column => leafLex (tName depth) column ++ [.raw "AS", .ident alias]
  | .group alias children => rowLex depth set children ++ [.raw "AS", .ident alias]
  | .nodes alias order _ =>
    [.raw "coalesce(json_agg(", .col (cName index) "j"] ++ orderLex (tName depth) order
      ++ [.raw "), '[]') AS", .ident alias]
  | .relation alias kind scan _ _ children =>
    if isSet set kind scan.limit scan.offset then
      [.raw "coalesce(", .col (cName index) "j", .raw ","]
        ++ (if kind = .aggregate then emptyRow children else [.raw "'[]'::json"])
        ++ [.raw ") AS", .ident alias]
    else [.col (cName index) "j", .raw "AS", .ident alias]
def fieldsFrom (depth index : Nat) (set : Bool) : List Node → List (List Lex)
  | [] => []
  | n :: ns => fieldLex depth index set n :: fieldsFrom depth (index + 1) set ns
def rowLex (depth : Nat) (set : Bool) (children : List Node) : List Lex :=
  [.raw "row_to_json((SELECT \"%\" FROM (SELECT"] ++ commas (fieldsFrom depth 0 set children)
    ++ [.raw ") AS \"%\"))"]
end

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
  | .object => ⟨scan.name, scan.schema, scan.table, scan.conds, [], some 1, none⟩
  | .array | .aggregate => scan

def lateralAgg (depth : Nat) (kind : Kind) (scan : Scan) (children : List Node) : List Lex :=
  match kind with
  | .object | .aggregate => rowLex depth false children
  | .array =>
    [.raw "coalesce(json_agg("] ++ rowLex depth false children ++ orderLex (tName depth) scan.order
      ++ [.raw "), '[]')"]

def resultLex (depth : Nat) (set : Bool) (kind : Kind) (scan : Scan) (children : List Node) : List Lex :=
  if kind = .aggregate then rowLex depth set children
  else [.raw "coalesce(json_agg("] ++ rowLex depth set children ++ orderLex (tName depth) scan.order
    ++ [.raw "), '[]')"]

mutual
def joinToks (depth index : Nat) (set : Bool) : Node → List Tok
  | .column _ _ => []
  | .group _ children => joinsToks depth 0 set children
  | .nodes _ _ children =>
    lift ([.raw "LEFT JOIN LATERAL (SELECT"] ++ rowLex depth set children
      ++ [.raw "AS \"j\" FROM (SELECT true) AS \"_\""])
      ++ joinsToks depth 0 set children
      ++ lift [.raw ") AS", .ident (cName index), .raw "ON true"]
  | .relation _ kind scan parent child children =>
    if isSet set kind scan.limit scan.offset then
      lift [.raw "LEFT JOIN (WITH", .ident (rName (depth + 1)), .raw "AS ("]
        ++ scanToks scan (setLink depth parent child)
        ++ lift ([.raw ") SELECT"] ++ keyLex (depth + 1) child ++ resultLex (depth + 1) true kind scan children
          ++ [.raw "AS \"j\" FROM", .ident (rName (depth + 1)), .raw "AS", .ident (tName (depth + 1))])
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
    ++ lift ([.raw ") SELECT"] ++ resultLex 0 true query.kind query.scan query.children
      ++ [.raw "FROM", .ident (rName 0), .raw "AS", .ident (tName 0)])
    ++ joinsToks 0 0 true query.children

def compile (query : Query) : Compiled := render (emit query)

def placeholders (query : Query) : List Nat := slots (emit query)

def scanValues (scan : Scan) : List Value := predicateValues scan.conds

mutual
def nodeValues : Node → List Value
  | .group _ children | .nodes _ _ children => nodesValues children
  | .column _ _ => []
  | .relation _ _ scan _ _ children => scanValues scan ++ nodesValues children
def nodesValues : List Node → List Value
  | [] => []
  | n :: ns => nodeValues n ++ nodesValues ns
end

def queryValues (query : Query) : List Value := scanValues query.scan ++ nodesValues query.children

def eraseScan (scan : Scan) : Scan := ⟨scan.name, scan.schema, scan.table, erasePredicate scan.conds, scan.order, scan.limit, scan.offset⟩

mutual
def eraseNode : Node → Node
  | .group alias children => .group alias (eraseNodes children)
  | .nodes alias order children => .nodes alias order (eraseNodes children)
  | .column alias column => .column alias column
  | .relation alias kind scan parent child children =>
    .relation alias kind (eraseScan scan) parent child (eraseNodes children)
def eraseNodes : List Node → List Node
  | [] => []
  | n :: ns => eraseNode n :: eraseNodes ns
end

def eraseQuery (query : Query) : Query :=
  { kind := query.kind, scan := eraseScan query.scan, children := eraseNodes query.children }

theorem scanToks_values (scan : Scan) (link : List Lex) :
    (scanToks scan link).filterMap tokValue = scanValues scan := by
  simp [scanToks, scanValues, lift_values, expressionToks_values]

theorem scanToks_erase (scan : Scan) (link : List Lex) :
    (scanToks scan link).map eraseTok = scanToks (eraseScan scan) link := by
  cases scan
  simp [scanToks, eraseScan, lift_erase, expressionToks_erase, Scan.schema, Scan.table, Scan.conds, Scan.order, Scan.limit, Scan.offset]

theorem lateralScan_values (kind : Kind) (scan : Scan) :
    scanValues (lateralScan kind scan) = scanValues scan := by
  cases kind <;> cases scan <;> rfl

theorem lateralScan_erase (kind : Kind) (scan : Scan) :
    eraseScan (lateralScan kind scan) = lateralScan kind (eraseScan scan) := by
  cases kind <;> cases scan <;> rfl

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
  | group alias children => simpa [joinToks, nodeValues] using joinsToks_values depth 0 set children
  | nodes alias order children => simp [joinToks, nodeValues, lift_values, joinsToks_values depth 0 set children]
theorem joinsToks_values (depth index : Nat) (set : Bool) (ns : List Node) :
    (joinsToks depth index set ns).filterMap tokValue = nodesValues ns := by
  cases ns with
  | nil => simp [joinsToks, nodesValues]
  | cons n ns =>
    simp [joinsToks, nodesValues, joinToks_values depth index set n,
      joinsToks_values depth (index + 1) set ns]
end

mutual
theorem emptyField_erase (n : Node) : emptyField (eraseNode n) = emptyField n := by
  cases n with
  | column alias leaf => rfl
  | relation alias kind scan parent child children => simp [emptyField, eraseNode]
  | nodes alias order children => simp [emptyField, eraseNode]
  | group alias children => simp [emptyField, eraseNode, emptyRow_erase children]
termination_by 2 * sizeOf n + 0
decreasing_by all_goals simp_wf <;> omega

theorem emptyFields_erase (ns : List Node) : emptyFields (eraseNodes ns) = emptyFields ns := by
  cases ns with
  | nil => rfl
  | cons n ns => simp [emptyFields, eraseNodes, emptyField_erase n, emptyFields_erase ns]
termination_by 2 * sizeOf ns + 0
decreasing_by all_goals simp_wf <;> omega

theorem emptyRow_erase (ns : List Node) : emptyRow (eraseNodes ns) = emptyRow ns := by
  simp [emptyRow, emptyFields_erase ns]
termination_by 2 * sizeOf ns + 1
decreasing_by all_goals simp_wf <;> omega

end

mutual
theorem fieldLex_erase (depth index : Nat) (set : Bool) (n : Node) :
    fieldLex depth index set (eraseNode n) = fieldLex depth index set n := by
  cases n with
  | column alias leaf => rfl
  | nodes alias order children => simp [fieldLex, eraseNode]
  | relation alias kind scan parent child children =>
    cases scan
    simp [eraseNode, fieldLex, eraseScan, emptyRow_erase, Scan.limit, Scan.offset]
    rfl
  | group alias children => simp [eraseNode, fieldLex, rowLex_erase depth set children]
termination_by 2 * sizeOf n + 0
decreasing_by all_goals simp_wf <;> omega

theorem fieldsFrom_erase (depth index : Nat) (set : Bool) (ns : List Node) :
    fieldsFrom depth index set (eraseNodes ns) = fieldsFrom depth index set ns := by
  cases ns with
  | nil => rfl
  | cons n ns => simp [eraseNodes, fieldsFrom, fieldLex_erase depth index set n,
      fieldsFrom_erase depth (index + 1) set ns]
termination_by 2 * sizeOf ns + 0
decreasing_by all_goals simp_wf <;> omega

theorem rowLex_erase (depth : Nat) (set : Bool) (ns : List Node) :
    rowLex depth set (eraseNodes ns) = rowLex depth set ns := by
  simp [rowLex, fieldsFrom_erase depth 0 set ns]
termination_by 2 * sizeOf ns + 1
decreasing_by all_goals simp_wf <;> omega

end

theorem resultLex_erase (depth : Nat) (set : Bool) (kind : Kind) (scan : Scan) (ns : List Node) :
    resultLex depth set kind (eraseScan scan) (eraseNodes ns) = resultLex depth set kind scan ns := by
  cases scan
  simp [resultLex, rowLex_erase, eraseScan, Scan.order]

theorem lateralAgg_erase (depth : Nat) (kind : Kind) (scan : Scan) (ns : List Node) :
    lateralAgg depth kind (eraseScan scan) (eraseNodes ns) = lateralAgg depth kind scan ns := by
  cases scan
  cases kind <;> simp [lateralAgg, rowLex_erase, eraseScan, Scan.order]

mutual
theorem joinToks_erase (depth index : Nat) (set : Bool) (n : Node) :
    (joinToks depth index set n).map eraseTok = joinToks depth index set (eraseNode n) := by
  cases n with
  | column alias column => simp [joinToks, eraseNode]
  | relation alias kind scan parent child children =>
    simp only [joinToks, eraseNode]
    have hlimit : (eraseScan scan).limit = scan.limit := rfl
    have hoffset : (eraseScan scan).offset = scan.offset := rfl
    rw [hlimit, hoffset]
    split <;>
      simp [lift_erase, scanToks_erase, lateralScan_erase, lateralAgg_erase,
        joinsToks_erase (depth + 1) 0 _ children, resultLex_erase]
  | group alias children => simpa [joinToks, eraseNode] using joinsToks_erase depth 0 set children
  | nodes alias order children => simp [joinToks, eraseNode, lift_erase, rowLex_erase, joinsToks_erase depth 0 set children]
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
  simp [emit, eraseQuery, lift_erase, scanToks_erase, joinsToks_erase, resultLex_erase]

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
    lateralScan .object scan = ⟨scan.name, scan.schema, scan.table, scan.conds, [], some 1, none⟩ := rfl

theorem lateralScan_limit (kind : Kind) (scan : Scan) :
    (lateralScan kind scan).limit = (if kind = .object then some 1 else scan.limit) := by
  cases kind <;> cases scan <;> rfl

theorem lateralScan_aggregate (scan : Scan) : lateralScan .aggregate scan = scan := rfl

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

inductive RootKind where
  | list | aggregate
  deriving DecidableEq, Repr

structure RootField where
  field : String
  target : String
  kind : RootKind
  deriving DecidableEq, Repr

inductive Operand where
  | literal (value : Value)
  | session (name : String)
  deriving DecidableEq, Repr

structure Session where
  role : String
  variables : List (String × Value)
  deriving DecidableEq, Repr

structure Permission where
  target : String
  role : String
  conds : Expression String Operand
  columns : List String
  aggregations : Bool
  deriving Repr

structure Schema where
  tables : List Table
  roots : List RootField
  permissions : List Permission
  deriving Repr

structure Order where
  field : String
  direction : Direction
  deriving DecidableEq, Repr

structure Selection where
  alias : String
  name : String
  conds : Option (Expression String Value)
  order : List Order
  limit : Option Nat
  offset : Option Nat
  children : List Selection
  deriving Repr

inductive Error where
  | unknownRoot (name : String)
  | unknownType (name : String)
  | unknownField (type field : String)
  | columnArguments (type field : String)
  | objectArguments (type field : String)
  | invalidRelation (type field : String)
  | invalidSelection (type : String)
  | invalidFilter
  | missingSession (name : String)
  | invalidPermission (name : String)
  | tooDeep
  deriving DecidableEq, Repr

def maxDepth : Nat := 32

def maxFields : Nat := 1000

def maxFilterDepth : Nat := 32

def maxInValues : Nat := 1000

def filterValues (f : Option (Expression String Value)) : List Value :=
  f.toList.flatMap expressionValues

def rowAlias : String := "%"

def findColumn (table : Table) (field : String) : Option String :=
  (table.columns.find? (·.field = field)).map (·.column)

def findField (table : Table) (name : String) : Option Leaf :=
  if name = "__typename" then some (.typename table.name)
  else (findColumn table name).map Leaf.column

def findRelation (table : Table) (field : String) : Option Relation :=
  table.relations.find? (·.field = field)

def findTable (schema : Schema) (name : String) : Option Table :=
  schema.tables.find? (·.name = name)

def scalarValid : Value → Bool
  | .null => false
  | .float bits => (bits / 4503599627370496) % 2048 != 2047
  | _ => true

def membershipValid (vs : List Value) : Bool :=
  vs.length ≤ maxInValues && vs.all scalarValid

def relationValid (relation : Relation) : Bool :=
  !relation.parent.isEmpty && relation.parent.length == relation.child.length

mutual
def resolveExpression (schema : Schema) (table : Table) (depth : Nat)
    (e : Expression String Value) : Except Error (Predicate) :=
    if maxFilterDepth < depth then .error .tooDeep
    else match e with
    | .compare field op v =>
      if !scalarValid v then .error .invalidFilter
      else match findColumn table field with
      | none => .error (.unknownField table.name field)
      | some column => .ok (.compare column op v)
    | .member field neg vs =>
      if !membershipValid vs then .error .invalidFilter
      else match findColumn table field with
      | none => .error (.unknownField table.name field)
      | some column => .ok (.member column neg vs)
    | .isNull field b =>
      match findColumn table field with
      | none => .error (.unknownField table.name field)
      | some column => .ok (.isNull column b)
    | .and es =>
      if maxFields < es.length then .error .invalidFilter
      else (resolveExpressions schema table (depth + 1) es).map Predicate.and
    | .or es =>
      if maxFields < es.length then .error .invalidFilter
      else (resolveExpressions schema table (depth + 1) es).map Predicate.or
    | .not e => (resolveExpression schema table (depth + 1) e).map Predicate.not
    | .relation name e =>
      match findRelation table name with
      | none => .error (.unknownField table.name name)
      | some relation =>
        if relation.kind = .aggregate then .error (.unknownField table.name name)
        else if !relationValid relation then .error (.invalidRelation table.name name)
        else match findTable schema relation.target with
        | none => .error (.unknownType relation.target)
        | some target =>
          (resolveExpression schema target (depth + 1) e).map fun out =>
            .exists ⟨target.name, target.schema, target.table, out, [], none, none⟩ relation.parent relation.child
termination_by sizeOf e
def resolveExpressions (schema : Schema) (table : Table) (depth : Nat)
    (es : List (Expression String Value)) : Except Error (List (Predicate)) :=
  match es with
  | [] => .ok []
  | e :: es =>
    match resolveExpression schema table depth e with
    | .error err => .error err
    | .ok out => (resolveExpressions schema table depth es).map (out :: ·)
termination_by sizeOf es
end

def resolveFilter (schema : Schema) (table : Table) :
    Option (Expression String Value) → Except Error (Predicate)
  | none => .ok (.and [])
  | some e => resolveExpression schema table 0 e

def resolveOrder (table : Table) : List Order → Except Error (List (String × Direction))
  | [] => .ok []
  | o :: os =>
    match findColumn table o.field with
    | none => .error (.unknownField table.name o.field)
    | some column =>
      match resolveOrder table os with
      | .error e => .error e
      | .ok rest => .ok ((column, o.direction) :: rest)

def resolveScan (schema : Schema) (table : Table) (conds : Option (Expression String Value)) (order : List Order) (limit offset : Option Nat) :
    Except Error Scan :=
  match resolveFilter schema table conds with
  | .error e => .error e
  | .ok conds =>
    match resolveOrder table order with
    | .error e => .error e
    | .ok order => .ok ⟨table.name, table.schema, table.table, conds, order, limit, offset⟩

def aliasValid (alias : String) : Bool :=
  alias ≠ "" && alias ≠ rowAlias && alias.utf8ByteSize ≤ 63

def distinct : List String → Bool
  | [] => true
  | a :: rest => !rest.contains a && distinct rest

def selectionValid (aliases : List String) : Bool :=
  !aliases.isEmpty && aliases.length ≤ maxFields && aliases.all aliasValid && distinct aliases

def plain (selection : Selection) : Bool :=
  selection.conds.isNone && selection.order.isEmpty && selection.limit.isNone && selection.offset.isNone

inductive Mode where
  | row | aggregate | summary | metric (op : AggregateOp)
  deriving DecidableEq, Repr

def relationMode : Kind → Mode
  | .aggregate => .aggregate
  | _ => .row

def summaryOp : String → Option AggregateOp
  | "sum" => some .sum | "avg" => some .avg | "min" => some .min | "max" => some .max
  | _ => none

def modeLeaf (table : Table) (mode : Mode) (name : String) : Option Leaf :=
  match mode with
  | .row => findField table name
  | .summary => if name = "count" then some .count else none
  | .metric op => (findColumn table name).map (.metric op)
  | .aggregate => none

def childMode (mode : Mode) (name : String) : Option Mode :=
  match mode with
  | .aggregate => if name = "aggregate" then some .summary else if name = "nodes" then some .row else none
  | .summary => (summaryOp name).map Mode.metric
  | _ => none

mutual
def resolveNode (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (scanOrder : List (String × Direction)) : Selection → Except Error Node
  | ⟨alias, name, conds, order, limit, offset, children⟩ =>
    match modeLeaf table mode name with
    | some column =>
      if conds.isNone && order.isEmpty && limit.isNone && offset.isNone && children.isEmpty then
        .ok (.column alias column)
      else .error (.columnArguments table.name name)
    | none =>
      match childMode mode name with
      | some next =>
        if maxDepth + 3 ≤ depth then .error .tooDeep
        else if !(conds.isNone && order.isEmpty && limit.isNone && offset.isNone) then
          .error (.columnArguments table.name name)
        else if !selectionValid (children.map (·.alias)) then .error (.invalidSelection table.name)
        else
          match resolveNodes schema table (depth + 1) next scanOrder children with
          | .error e => .error e
          | .ok nodes => .ok (if mode = .aggregate && name = "nodes" then .nodes alias scanOrder nodes else .group alias nodes)
      | none =>
        if mode != .row then .error (.unknownField table.name name)
        else match findRelation table name with
        | none => .error (.unknownField table.name name)
        | some relation =>
          match findTable schema relation.target with
          | none => .error (.unknownType relation.target)
          | some target =>
            if maxDepth ≤ depth then .error .tooDeep
            else if relation.kind = .object && !(conds.isNone && order.isEmpty && limit.isNone && offset.isNone) then
              .error (.objectArguments table.name name)
            else if !relationValid relation then .error (.invalidRelation table.name name)
            else if !selectionValid (children.map (·.alias)) then .error (.invalidSelection target.name)
            else
              match resolveScan schema target conds order limit offset with
              | .error e => .error e
              | .ok scan =>
                match resolveNodes schema target (depth + 1) (relationMode relation.kind) scan.order children with
                | .error e => .error e
                | .ok nodes => .ok (.relation alias relation.kind scan relation.parent relation.child nodes)
def resolveNodes (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (scanOrder : List (String × Direction)) : List Selection → Except Error (List Node)
  | [] => .ok []
  | s :: ss =>
    match resolveNode schema table depth mode scanOrder s with
    | .error e => .error e
    | .ok node =>
      match resolveNodes schema table depth mode scanOrder ss with
      | .error e => .error e
      | .ok nodes => .ok (node :: nodes)
end

def resolveSelection (schema : Schema) (root : Selection) : Except Error Query :=
  match schema.roots.find? (·.field = root.name) with
  | none => .error (.unknownRoot root.name)
  | some field =>
    match findTable schema field.target with
    | none => .error (.unknownType field.target)
    | some table =>
      if !selectionValid (root.children.map (·.alias)) then .error (.invalidSelection table.name)
      else
        match resolveScan schema table root.conds root.order root.limit root.offset with
        | .error e => .error e
        | .ok scan =>
          let kind := if field.kind = .aggregate then Kind.aggregate else Kind.array
          match resolveNodes schema table 0 (relationMode kind) scan.order root.children with
          | .error e => .error e
          | .ok children => .ok { kind, scan, children }

def runSelection (schema : Schema) (root : Selection) : Except Error Compiled :=
  match resolveSelection schema root with
  | .error e => .error e
  | .ok query => .ok (compile query)

mutual
def selectionValues : Selection → List Value
  | ⟨_, _, conds, _, _, _, children⟩ => filterValues conds ++ selectionsValues children
def selectionsValues : List Selection → List Value
  | [] => []
  | s :: ss => selectionValues s ++ selectionsValues ss
end

mutual
theorem resolveExpression_values (schema : Schema) (table : Table) (depth : Nat)
    (e : Expression String Value) (out : Predicate)
    (h : resolveExpression schema table depth e = .ok out) :
    predicateValues out = expressionValues e := by
  cases e <;> simp only [resolveExpression] at h
  all_goals repeat' first | cases h | split at h
  all_goals try simp only [Except.map] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp only [expressionValues, predicateValues]
  all_goals first
    | rfl
    | exact resolveExpression_values _ _ _ _ _ (by assumption)
    | exact resolveExpressions_values _ _ _ _ _ (by assumption)
theorem resolveExpressions_values (schema : Schema) (table : Table) (depth : Nat)
    (es : List (Expression String Value)) (out : List (Predicate))
    (h : resolveExpressions schema table depth es = .ok out) :
    predicatesValues out = expressionsValues es := by
  cases es <;> simp only [resolveExpressions] at h
  all_goals repeat' first | cases h | split at h
  all_goals try simp only [Except.map] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp only [expressionsValues, predicatesValues]
  all_goals first
    | rfl
    | rw [resolveExpression_values _ _ _ _ _ (by assumption),
        resolveExpressions_values _ _ _ _ _ (by assumption)]
end

theorem resolveFilter_values (schema : Schema) (table : Table) (f : Option (Expression String Value))
    (out : Predicate) (h : resolveFilter schema table f = .ok out) :
    predicateValues out = filterValues f := by
  cases f with
  | none => cases h; rfl
  | some e => simpa [resolveFilter, filterValues] using resolveExpression_values schema table 0 e out h

theorem resolveScan_values (schema : Schema) (table : Table) (conds : Option (Expression String Value)) (order : List Order)
    (limit offset : Option Nat) (scan : Scan) (h : resolveScan schema table conds order limit offset = .ok scan) :
    scanValues scan = filterValues conds := by
  simp only [resolveScan] at h
  split at h
  · cases h
  · rename_i out hout
    split at h
    · cases h
    · cases h
      simpa [scanValues, Scan.conds] using resolveFilter_values schema table conds out hout

mutual
theorem resolveNode_values (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (scanOrder : List (String × Direction)) (s : Selection)
    (n : Node) (h : resolveNode schema table depth mode scanOrder s = .ok n) :
    nodeValues n = selectionValues s := by
  obtain ⟨alias, name, conds, order, limit, offset, children⟩ := s
  simp only [resolveNode] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp_all
  all_goals try split
  all_goals simp only [nodeValues, selectionValues, filterValues, Option.toList_none, List.flatMap_nil,
    List.nil_append, selectionsValues]
  all_goals first
    | rfl
    | exact resolveNodes_values _ _ _ _ _ _ _ (by assumption)
    | rw [resolveScan_values _ _ _ _ _ _ _ (by assumption),
        resolveNodes_values _ _ _ _ _ _ _ (by assumption)]; rfl
theorem resolveNodes_values (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (scanOrder : List (String × Direction)) (ss : List Selection)
    (ns : List Node) (h : resolveNodes schema table depth mode scanOrder ss = .ok ns) :
    nodesValues ns = selectionsValues ss := by
  cases ss <;> simp only [resolveNodes] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp only [nodesValues, selectionsValues]
  all_goals first
    | rfl
    | rw [resolveNode_values _ _ _ _ _ _ _ (by assumption),
        resolveNodes_values _ _ _ _ _ _ _ (by assumption)]
end

theorem resolveSelection_values (schema : Schema) (root : Selection) :
    match resolveSelection schema root with
    | .ok query => queryValues query = selectionValues root
    | .error _ => True := by
  split
  · rename_i query h
    simp only [resolveSelection] at h
    repeat' first | cases h | split at h
    simp only [queryValues]
    rw [resolveScan_values _ _ _ _ _ _ _ (by assumption),
      resolveNodes_values _ _ _ _ _ _ _ (by assumption)]
    cases root; rfl
  · trivial

theorem runSelection_params (schema : Schema) (root : Selection) :
    match runSelection schema root with
    | .ok compiled => compiled.params = selectionValues root
    | .error _ => True := by
  have h := resolveSelection_values schema root
  simp only [runSelection]
  split at h
  · rename_i query hq
    simp [hq, compile_params, h]
  · rename_i e hq
    simp [hq]

def referenceDeclared (schema : Schema) (ref : Reference) : Bool :=
  schema.tables.any fun t => t.schema = ref.schema && t.table = ref.table

mutual
def expressionDeclared (schema : Schema) : Predicate → Bool
  | .compare _ _ _ | .member _ _ _ | .isNull _ _ => true
  | .and es | .or es => expressionsDeclared schema es
  | .not e => expressionDeclared schema e
  | .exists (.mk _ ns table e _ _ _) parent child => referenceDeclared schema ⟨ns, table, parent, child⟩ && expressionDeclared schema e
def expressionsDeclared (schema : Schema) : List (Predicate) → Bool
  | [] => true
  | e :: es => expressionDeclared schema e && expressionsDeclared schema es
end

theorem found_reference (schema : Schema) (name : String) (table : Table)
    (parent child : List String) (h : findTable schema name = some table) :
    referenceDeclared schema ⟨table.schema, table.table, parent, child⟩ = true := by
  have hmem := List.mem_of_find?_eq_some h
  exact List.any_eq_true.mpr ⟨table, hmem, by simp⟩

mutual
theorem resolveExpression_declared (schema : Schema) (table : Table) (depth : Nat)
    (e : Expression String Value) (out : Predicate)
    (h : resolveExpression schema table depth e = .ok out) :
    expressionDeclared schema out = true := by
  cases e <;> simp only [resolveExpression] at h
  all_goals repeat' first | cases h | split at h
  all_goals try simp only [Except.map] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp only [expressionDeclared, Bool.and_eq_true]
  all_goals first
    | rfl
    | exact resolveExpression_declared _ _ _ _ _ (by assumption)
    | exact resolveExpressions_declared _ _ _ _ _ (by assumption)
    | exact ⟨found_reference _ _ _ _ _ (by assumption),
        resolveExpression_declared _ _ _ _ _ (by assumption)⟩
theorem resolveExpressions_declared (schema : Schema) (table : Table) (depth : Nat)
    (es : List (Expression String Value)) (out : List (Predicate))
    (h : resolveExpressions schema table depth es = .ok out) :
    expressionsDeclared schema out = true := by
  cases es <;> simp only [resolveExpressions] at h
  all_goals repeat' first | cases h | split at h
  all_goals try simp only [Except.map] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp only [expressionsDeclared, Bool.and_eq_true]
  all_goals first
    | rfl
    | exact ⟨resolveExpression_declared _ _ _ _ _ (by assumption),
        resolveExpressions_declared _ _ _ _ _ (by assumption)⟩
end

theorem resolveFilter_declared (schema : Schema) (table : Table) (f : Option (Expression String Value))
    (out : Predicate) (h : resolveFilter schema table f = .ok out) :
    expressionDeclared schema out = true := by
  cases f with
  | none => cases h; rfl
  | some e => exact resolveExpression_declared schema table 0 e out h

def declared (schema : Schema) (scan : Scan) : Bool :=
  referenceDeclared schema ⟨scan.schema, scan.table, [], []⟩ && expressionDeclared schema scan.conds

mutual
def nodeScans : Node → List Scan
  | .group _ children | .nodes _ _ children => nodesScans children
  | .column _ _ => []
  | .relation _ _ scan _ _ children => scan :: nodesScans children
def nodesScans : List Node → List Scan
  | [] => []
  | n :: ns => nodeScans n ++ nodesScans ns
end

def queryScans (query : Query) : List Scan := query.scan :: nodesScans query.children

theorem findTable_declared (schema : Schema) (name : String) (table : Table)
    (conds : Option (Expression String Value)) (order : List Order) (limit offset : Option Nat) (scan : Scan)
    (hfind : findTable schema name = some table)
    (hscan : resolveScan schema table conds order limit offset = .ok scan) : declared schema scan = true := by
  simp only [resolveScan] at hscan
  split at hscan
  · cases hscan
  · rename_i out hout
    split at hscan
    · cases hscan
    · cases hscan
      simp only [declared, Scan.schema, Scan.table, Scan.conds, Bool.and_eq_true]
      exact ⟨found_reference _ _ _ _ _ hfind, resolveFilter_declared _ _ _ _ hout⟩

mutual
theorem resolveNode_declared (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (scanOrder : List (String × Direction)) (s : Selection)
    (n : Node) (h : resolveNode schema table depth mode scanOrder s = .ok n) :
    (nodeScans n).all (declared schema) = true := by
  obtain ⟨alias, name, conds, order, limit, offset, children⟩ := s
  simp only [resolveNode] at h
  all_goals repeat' first | cases h | split at h
  all_goals try split
  all_goals simp only [nodeScans, List.all_nil, List.all_cons, Bool.and_eq_true]
  all_goals first
    | rfl
    | exact resolveNodes_declared _ _ _ _ _ _ _ (by assumption)
    | exact ⟨findTable_declared _ _ _ _ _ _ _ _ (by assumption) (by assumption),
        resolveNodes_declared _ _ _ _ _ _ _ (by assumption)⟩
theorem resolveNodes_declared (schema : Schema) (table : Table) (depth : Nat) (mode : Mode)
    (scanOrder : List (String × Direction)) (ss : List Selection)
    (ns : List Node) (h : resolveNodes schema table depth mode scanOrder ss = .ok ns) :
    (nodesScans ns).all (declared schema) = true := by
  cases ss <;> simp only [resolveNodes] at h
  all_goals repeat' first | cases h | split at h
  all_goals simp only [nodesScans, List.all_nil, List.all_append, Bool.and_eq_true]
  all_goals first
    | rfl
    | exact ⟨resolveNode_declared _ _ _ _ _ _ _ (by assumption),
        resolveNodes_declared _ _ _ _ _ _ _ (by assumption)⟩
end

theorem resolveSelection_declared (schema : Schema) (root : Selection) :
    match resolveSelection schema root with
    | .ok query => (queryScans query).all (declared schema) = true
    | .error _ => True := by
  split
  · rename_i query h
    simp only [resolveSelection] at h
    repeat' first | cases h | split at h
    simp only [queryScans, List.all_cons, Bool.and_eq_true]
    exact ⟨findTable_declared _ _ _ _ _ _ _ _ (by assumption) (by assumption),
      resolveNodes_declared _ _ _ _ _ _ _ (by assumption)⟩
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

theorem scalarValid_iff (v : Value) : scalarValid v = true ↔
    (v != .null && (match v with
      | .float bits => (bits / 4503599627370496) % 2048 != 2047
      | _ => true)) = true := by
  cases v <;> simp [scalarValid]

theorem membershipValid_iff (vs : List Value) : membershipValid vs = true ↔
    vs.length ≤ maxInValues ∧ ∀ v ∈ vs, scalarValid v = true := by
  simp [membershipValid]

theorem relationValid_iff (relation : Relation) :
    relationValid relation = true ↔
      relation.parent ≠ [] ∧ relation.parent.length = relation.child.length := by
  simp [relationValid]
def findPermission (schema : Schema) (role target : String) : Option Permission :=
  schema.permissions.find? fun p => p.role == role && p.target == target

def tableVisible (schema : Schema) (role target : String) : Bool :=
  role == "admin" || (findPermission schema role target).isSome

def aggregateVisible (schema : Schema) (role target : String) : Bool :=
  role == "admin" || ((findPermission schema role target).map (·.aggregations)).getD false

def visibleSchema (schema : Schema) (role : String) : Schema :=
  if role == "admin" then schema else
  { schema with
    tables := schema.tables.filterMap fun t =>
      (findPermission schema role t.name).map fun p => { t with
        columns := t.columns.filter fun c => p.columns.contains c.field
        relations := t.relations.filter fun r => tableVisible schema role r.target &&
          (r.kind != .aggregate || aggregateVisible schema role r.target) }
    roots := schema.roots.filter fun r => tableVisible schema role r.target &&
      (r.kind != .aggregate || aggregateVisible schema role r.target) }

def bindOperand (session : Session) : Operand → Except Error Value
  | .literal value => .ok value
  | .session name => match session.variables.find? (·.1 == name) with
    | some (_, value) => .ok value
    | none => .error (.missingSession name)

def bindOperands (session : Session) : List Operand → Except Error (List Value)
  | [] => .ok []
  | v :: vs => do return (← bindOperand session v) :: (← bindOperands session vs)

mutual
def bindExpression (session : Session) (depth : Nat) (e : Expression String Operand) :
    Except Error (Expression String Value) :=
  if maxFilterDepth < depth then .error .tooDeep else
  match e with
  | .compare field op value => (bindOperand session value).map (.compare field op)
  | .member field neg values =>
    if maxInValues < values.length then .error .invalidFilter
    else (bindOperands session values).map (.member field neg)
  | .isNull field value => .ok (.isNull field value)
  | .and es =>
    if maxFields < es.length then .error .invalidFilter
    else (bindExpressions session (depth + 1) es).map .and
  | .or es =>
    if maxFields < es.length then .error .invalidFilter
    else (bindExpressions session (depth + 1) es).map .or
  | .not e => (bindExpression session (depth + 1) e).map .not
  | .relation ref e => (bindExpression session (depth + 1) e).map (.relation ref)
def bindExpressions (session : Session) (depth : Nat) : List (Expression String Operand) →
    Except Error (List (Expression String Value))
  | [] => .ok []
  | e :: es => do return (← bindExpression session depth e) :: (← bindExpressions session depth es)
end

def roleCondition (schema : Schema) (session : Session) (name : String) : Except Error Predicate := do
  let table ← match findTable schema name with
    | some table => .ok table
    | none => .error (.unknownType name)
  let permission ← match findPermission schema session.role name with
    | some permission => .ok permission
    | none => .error (.unknownType name)
  let input ← bindExpression session 0 permission.conds
  resolveExpression schema table 0 input

mutual
def predicateEq : Predicate → Predicate → Bool
  | .compare f op v, .compare g oq w => f == g && op == oq && v == w
  | .member f n vs, .member g m ws => f == g && n == m && vs == ws
  | .isNull f b, .isNull g c => f == g && b == c
  | .and es, .and fs | .or es, .or fs => predicatesEq es fs
  | .not e, .not f => predicateEq e f
  | .exists (.mk name ns t e os l o) ps cs, .exists (.mk name' ns' t' f os' l' o') ps' cs' =>
    name == name' && ns == ns' && t == t' && predicateEq e f && os == os' && l == l' && o == o' && ps == ps' && cs == cs'
  | _, _ => false
def predicatesEq : List Predicate → List Predicate → Bool
  | [], [] => true
  | e :: es, f :: fs => predicateEq e f && predicatesEq es fs
  | _, _ => false
end

mutual
theorem predicateEq_iff (e f : Predicate) : predicateEq e f = true ↔ e = f := by
  cases e <;> cases f
  all_goals try (rename_i s ps cs t qs ds; cases s; cases t)
  all_goals simp [predicateEq, predicateEq_iff, predicatesEq_iff, and_assoc]
theorem predicatesEq_iff (es fs : List Predicate) : predicatesEq es fs = true ↔ es = fs := by
  cases es <;> cases fs <;> simp [predicatesEq, predicateEq_iff, predicatesEq_iff]
end

def hasConjunct (condition expression : Predicate) : Bool :=
  predicateEq expression condition || match expression with
    | .and es => es.any fun e => predicateEq e condition
    | _ => false

def scanPermitted (schema : Schema) (session : Session) (scan : Scan) : Bool :=
  match findTable schema scan.name with
  | none => false
  | some table => table.schema == scan.schema && table.table == scan.table &&
    (session.role == "admin" || match roleCondition schema session scan.name with
      | .error _ => false
      | .ok condition => hasConjunct condition scan.conds)

mutual
def predicateScans : Predicate → List Scan
  | .compare _ _ _ | .member _ _ _ | .isNull _ _ => []
  | .and es | .or es => predicatesScans es
  | .not e => predicateScans e
  | .exists (.mk name ns t e os l o) _ _ => .mk name ns t e os l o :: predicateScans e
def predicatesScans : List Predicate → List Scan
  | [] => []
  | e :: es => predicateScans e ++ predicatesScans es
end

def scanTree (scan : Scan) : List Scan := scan :: predicateScans scan.conds

def allScans (query : Query) : List Scan := (queryScans query).flatMap scanTree

def columnPermitted (schema : Schema) (session : Session) (name column : String) : Bool :=
  match findTable (visibleSchema schema session.role) name with
  | none => false
  | some table => table.columns.any (·.column == column)

mutual
def predicateReads (name : String) : Predicate → List (String × String)
  | .compare field _ _ | .member field _ _ | .isNull field _ => [(name, field)]
  | .and es | .or es => predicatesReads name es
  | .not e => predicateReads name e
  | .exists scan _ _ => scanReads scan
def predicatesReads (name : String) : List Predicate → List (String × String)
  | [] => []
  | e :: es => predicateReads name e ++ predicatesReads name es
def scanReads : Scan → List (String × String)
  | .mk name _ _ (.and [user, _]) order _ _ =>
    predicateReads name user ++ order.map (fun o => (name, o.1))
  | .mk name _ _ e order _ _ => predicateReads name e ++ order.map (fun o => (name, o.1))
end

def leafReads (name : String) : Leaf → List (String × String)
  | .column c | .metric _ c => [(name, c)]
  | _ => []

mutual
def nodeReads (name : String) : Node → List (String × String)
  | .column _ leaf => leafReads name leaf
  | .group _ children => nodesReads name children
  | .nodes _ order children => order.map (fun o => (name, o.1)) ++ nodesReads name children
  | .relation _ _ scan _ _ children => scanReads scan ++ nodesReads scan.name children
def nodesReads (name : String) : List Node → List (String × String)
  | [] => []
  | n :: ns => nodeReads name n ++ nodesReads name ns
end

def queryReads (query : Query) : List (String × String) :=
  scanReads query.scan ++ nodesReads query.scan.name query.children

def queryPermitted (schema : Schema) (session : Session) (query : Query) : Bool :=
  (allScans query).all (scanPermitted schema session) &&
    (queryReads query).all (fun c => columnPermitted schema session c.1 c.2)

def appendCondition (scan : Scan) (condition : Predicate) : Scan :=
  ⟨scan.name, scan.schema, scan.table, .and [scan.conds, condition], scan.order, scan.limit, scan.offset⟩

theorem appendCondition_exact (scan : Scan) (condition : Predicate) :
    appendCondition scan condition =
      .mk scan.name scan.schema scan.table (.and [scan.conds, condition]) scan.order scan.limit scan.offset := by
  rfl

def addPermission (schema : Schema) (session : Session) (scan : Scan) : Except Error Scan := do
  let condition ← roleCondition schema session scan.name
  return appendCondition scan condition

mutual
def guardPredicate (schema : Schema) (session : Session) : Predicate → Except Error Predicate
  | .compare f op v => .ok (.compare f op v)
  | .member f n vs => .ok (.member f n vs)
  | .isNull f b => .ok (.isNull f b)
  | .and es => (guardPredicates schema session es).map .and
  | .or es => (guardPredicates schema session es).map .or
  | .not e => (guardPredicate schema session e).map .not
  | .exists scan ps cs => (guardScan schema session scan).map fun s => .exists s ps cs
def guardPredicates (schema : Schema) (session : Session) : List Predicate → Except Error (List Predicate)
  | [] => .ok []
  | e :: es => do return (← guardPredicate schema session e) :: (← guardPredicates schema session es)
def guardScan (schema : Schema) (session : Session) : Scan → Except Error Scan
  | .mk name ns t e os l o => do
    let e ← guardPredicate schema session e
    addPermission schema session ⟨name, ns, t, e, os, l, o⟩
end

mutual
def guardNode (schema : Schema) (session : Session) : Node → Except Error Node
  | .column a c => .ok (.column a c)
  | .group a ns => (guardNodes schema session ns).map (.group a)
  | .nodes a os ns => (guardNodes schema session ns).map (.nodes a os)
  | .relation a k s ps cs ns => do
    let s ← guardScan schema session s
    let ns ← guardNodes schema session ns
    return .relation a k s ps cs ns
def guardNodes (schema : Schema) (session : Session) : List Node → Except Error (List Node)
  | [] => .ok []
  | n :: ns => do return (← guardNode schema session n) :: (← guardNodes schema session ns)
end

def guardQuery (schema : Schema) (session : Session) (query : Query) : Except Error Query := do
  let scan ← guardScan schema session query.scan
  let children ← guardNodes schema session query.children
  return { query with scan, children }

def resolve (schema : Schema) (session : Session) (root : Selection) : Except Error Query := do
  let query ← resolveSelection (visibleSchema schema session.role) root
  let query ← if session.role == "admin" then .ok query else guardQuery schema session query
  if queryPermitted schema session query then return query
  else .error (.invalidPermission root.name)

def run (schema : Schema) (session : Session) (root : Selection) : Except Error Compiled :=
  (resolve schema session root).map compile

theorem addPermission_condition (schema : Schema) (session : Session) (scan : Scan) :
    addPermission schema session scan = (roleCondition schema session scan.name).map
      (fun condition => .mk scan.name scan.schema scan.table (.and [scan.conds, condition]) scan.order scan.limit scan.offset) := by
  simp only [addPermission]
  cases roleCondition schema session scan.name <;> rfl

theorem resolve_permissions (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) :
    (allScans query).all (scanPermitted schema session) = true ∧
      (queryReads query).all (fun c => columnPermitted schema session c.1 c.2) = true := by
  simp only [resolve, bind, Except.bind] at h
  repeat' first | split at h | cases h
  all_goals simp_all [queryPermitted]
theorem hasConjunct_iff (condition expression : Predicate) :
    hasConjunct condition expression = true ↔
      expression = condition ∨ ∃ es, expression = .and es ∧ condition ∈ es := by
  cases expression <;> simp [hasConjunct, predicateEq_iff]

theorem scanPermitted_condition (schema : Schema) (session : Session) (scan : Scan)
    (h : scanPermitted schema session scan = true) (role : session.role ≠ "admin") :
    ∃ condition, roleCondition schema session scan.name = .ok condition ∧
      (scan.conds = condition ∨ ∃ es, scan.conds = .and es ∧ condition ∈ es) := by
  simp only [scanPermitted] at h
  split at h
  · cases h
  · simp only [Bool.and_eq_true, Bool.or_eq_true, beq_iff_eq] at h
    rcases h.2 with hr | hc
    · exact False.elim (role hr)
    · split at hc
      · cases hc
      · exact ⟨_, by assumption, (hasConjunct_iff _ _).mp hc⟩

theorem resolve_scan_conditions (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ scan ∈ allScans query, ∃ condition,
      roleCondition schema session scan.name = .ok condition ∧
      (scan.conds = condition ∨ ∃ es, scan.conds = .and es ∧ condition ∈ es) := by
  intro scan member
  exact scanPermitted_condition schema session scan
    (List.all_eq_true.mp (resolve_permissions schema session root query h).1 scan member) role

theorem resolve_columns_checked (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (_role : session.role ≠ "admin") :
    ∀ read ∈ queryReads query, columnPermitted schema session read.1 read.2 = true := by
  exact List.all_eq_true.mp (resolve_permissions schema session root query h).2

theorem resolve_tables_checked (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ scan ∈ allScans query, (findPermission schema session.role scan.name).isSome = true := by
  intro scan member
  obtain ⟨condition, hc, _⟩ := resolve_scan_conditions schema session root query h role scan member
  simp only [roleCondition, bind, Except.bind] at hc
  repeat' first | split at hc | cases hc
  all_goals simp_all
def roleValues (schema : Schema) (session : Session) (name : String) : List Value :=
  match roleCondition schema session name with
  | .ok condition => predicateValues condition
  | .error _ => []

mutual
def guardedPredicateValues (schema : Schema) (session : Session) : Predicate → List Value
  | .compare _ _ v => [v]
  | .member _ _ vs => vs
  | .isNull _ _ => []
  | .and es | .or es => guardedPredicatesValues schema session es
  | .not e => guardedPredicateValues schema session e
  | .exists scan _ _ => guardedScanValues schema session scan
def guardedPredicatesValues (schema : Schema) (session : Session) : List Predicate → List Value
  | [] => []
  | e :: es => guardedPredicateValues schema session e ++ guardedPredicatesValues schema session es
def guardedScanValues (schema : Schema) (session : Session) : Scan → List Value
  | .mk name _ _ e _ _ _ => guardedPredicateValues schema session e ++ roleValues schema session name
end

mutual
def guardedNodeValues (schema : Schema) (session : Session) : Node → List Value
  | .column _ _ => []
  | .group _ ns | .nodes _ _ ns => guardedNodesValues schema session ns
  | .relation _ _ scan _ _ ns => guardedScanValues schema session scan ++ guardedNodesValues schema session ns
def guardedNodesValues (schema : Schema) (session : Session) : List Node → List Value
  | [] => []
  | n :: ns => guardedNodeValues schema session n ++ guardedNodesValues schema session ns
end

def guardedQueryValues (schema : Schema) (session : Session) (query : Query) : List Value :=
  guardedScanValues schema session query.scan ++ guardedNodesValues schema session query.children

def documentValues (schema : Schema) (session : Session) (root : Selection) : List Value :=
  if session.role == "admin" then selectionValues root else
  match resolveSelection (visibleSchema schema session.role) root with
  | .ok query => guardedQueryValues schema session query
  | .error _ => []

theorem addPermission_values (schema : Schema) (session : Session) (scan out : Scan)
    (h : addPermission schema session scan = .ok out) :
    scanValues out = scanValues scan ++ roleValues schema session scan.name := by
  simp only [addPermission, bind, Except.bind] at h
  split at h
  · cases h
  · cases h
    simp_all [appendCondition, scanValues, Scan.conds, roleValues, predicateValues, predicatesValues]

mutual
theorem guardPredicate_values (schema : Schema) (session : Session) (e out : Predicate)
    (h : guardPredicate schema session e = .ok out) :
    predicateValues out = guardedPredicateValues schema session e := by
  cases e <;> simp only [guardPredicate, Except.map] at h
  all_goals repeat' first | split at h | cases h
  all_goals simp only [predicateValues, guardedPredicateValues]
  all_goals try change scanValues _ = guardedScanValues _ _ _
  all_goals first
    | rfl
    | exact guardPredicates_values _ _ _ _ (by assumption)
    | exact guardPredicate_values _ _ _ _ (by assumption)
    | (rename_i scan ps cs out heq; cases out; exact guardScan_values _ _ _ _ heq)
theorem guardPredicates_values (schema : Schema) (session : Session) (es out : List Predicate)
    (h : guardPredicates schema session es = .ok out) :
    predicatesValues out = guardedPredicatesValues schema session es := by
  cases es <;> simp only [guardPredicates, bind, Except.bind, pure, Except.pure] at h
  all_goals repeat' first | split at h | cases h
  all_goals simp only [predicatesValues, guardedPredicatesValues]
  all_goals first
    | rfl
    | rw [guardPredicate_values _ _ _ _ (by assumption), guardPredicates_values _ _ _ _ (by assumption)]
theorem guardScan_values (schema : Schema) (session : Session) (scan out : Scan)
    (h : guardScan schema session scan = .ok out) :
    scanValues out = guardedScanValues schema session scan := by
  cases scan
  simp only [guardScan, bind, Except.bind] at h
  split at h
  · cases h
  · rw [addPermission_values _ _ _ _ h]
    simp only [scanValues, Scan.conds, Scan.name, guardedScanValues]
    rw [guardPredicate_values _ _ _ _ (by assumption)]
end

mutual
theorem guardNode_values (schema : Schema) (session : Session) (n out : Node)
    (h : guardNode schema session n = .ok out) :
    nodeValues out = guardedNodeValues schema session n := by
  cases n <;> simp only [guardNode, Except.map, bind, Except.bind, pure, Except.pure] at h
  all_goals repeat' first | split at h | cases h
  all_goals simp only [nodeValues, guardedNodeValues]
  all_goals first
    | rfl
    | exact guardNodes_values _ _ _ _ (by assumption)
    | rw [guardScan_values _ _ _ _ (by assumption), guardNodes_values _ _ _ _ (by assumption)]
theorem guardNodes_values (schema : Schema) (session : Session) (ns out : List Node)
    (h : guardNodes schema session ns = .ok out) :
    nodesValues out = guardedNodesValues schema session ns := by
  cases ns <;> simp only [guardNodes, bind, Except.bind, pure, Except.pure] at h
  all_goals repeat' first | split at h | cases h
  all_goals simp only [nodesValues, guardedNodesValues]
  all_goals first
    | rfl
    | rw [guardNode_values _ _ _ _ (by assumption), guardNodes_values _ _ _ _ (by assumption)]
end

theorem guardQuery_values (schema : Schema) (session : Session) (query out : Query)
    (h : guardQuery schema session query = .ok out) :
    queryValues out = guardedQueryValues schema session query := by
  simp only [guardQuery, bind, Except.bind, pure, Except.pure] at h
  repeat' first | split at h | cases h
  simp only [queryValues, guardedQueryValues]
  rw [guardScan_values _ _ _ _ (by assumption), guardNodes_values _ _ _ _ (by assumption)]

theorem resolve_values (schema : Schema) (session : Session) (root : Selection) :
    match resolve schema session root with
    | .ok query => queryValues query = documentValues schema session root
    | .error _ => True := by
  split
  · rename_i query h
    simp only [resolve, bind, Except.bind] at h
    repeat' first | split at h | cases h
    all_goals simp only [documentValues]
    all_goals split
    all_goals first
      | (simpa [*] using resolveSelection_values (visibleSchema schema session.role) root)
      | (simp_all <;> exact guardQuery_values _ _ _ _ (by assumption))
  · trivial

theorem run_params (schema : Schema) (session : Session) (root : Selection) :
    match run schema session root with
    | .ok compiled => compiled.params = documentValues schema session root
    | .error _ => True := by
  have h := resolve_values schema session root
  simp only [run]
  split at h
  · rename_i query hq
    simp [hq, Except.map, compile_params, h]
  · rename_i e hq
    simp [hq, Except.map]

theorem resolve_declared (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) :
    ∀ scan ∈ allScans query, ∃ table ∈ schema.tables,
      table.name = scan.name ∧ table.schema = scan.schema ∧ table.table = scan.table := by
  intro scan member
  have permitted := List.all_eq_true.mp (resolve_permissions schema session root query h).1 scan member
  simp only [scanPermitted] at permitted
  split at permitted
  · cases permitted
  · rename_i table found
    have mem := List.mem_of_find?_eq_some found
    have name := List.find?_some found
    simp only [Bool.and_eq_true, beq_iff_eq] at permitted
    exact ⟨table, mem, by simpa using name, permitted.1.1, permitted.1.2⟩

theorem columnPermitted_origin (schema : Schema) (session : Session) (name column : String)
    (h : columnPermitted schema session name column = true) (role : session.role ≠ "admin") :
    ∃ table ∈ schema.tables, table.name = name ∧
      ∃ permission ∈ schema.permissions, permission.role = session.role ∧ permission.target = name ∧
        ∃ field ∈ table.columns, field.field ∈ permission.columns ∧ field.column = column := by
  simp only [columnPermitted] at h
  split at h
  · cases h
  · rename_i visible found
    have member := List.mem_of_find?_eq_some found
    have named := List.find?_some found
    simp only [visibleSchema, beq_iff_eq, role, ite_false] at member
    obtain ⟨table, tableMember, mapped⟩ := List.mem_filterMap.mp member
    cases granted : findPermission schema session.role table.name with
    | none => simp [granted] at mapped
    | some permission =>
      simp only [granted, Option.map_some, Option.some.injEq] at mapped
      cases mapped
      have grantMember := List.mem_of_find?_eq_some granted
      have grantNames := List.find?_some granted
      simp only [Bool.and_eq_true, beq_iff_eq] at grantNames
      obtain ⟨field, fieldMember, columnName⟩ := List.any_eq_true.mp h
      simp only [List.mem_filter, List.contains_iff_mem, beq_iff_eq] at fieldMember columnName
      exact ⟨table, tableMember, by simpa using named, permission, grantMember, grantNames.1,
        by simpa using grantNames.2.trans (by simpa using named), field, fieldMember.1, fieldMember.2, columnName⟩

theorem resolve_columns (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ read ∈ queryReads query, ∃ table ∈ schema.tables, table.name = read.1 ∧
      ∃ permission ∈ schema.permissions, permission.role = session.role ∧ permission.target = read.1 ∧
        ∃ field ∈ table.columns, field.field ∈ permission.columns ∧ field.column = read.2 := by
  intro read member
  exact columnPermitted_origin schema session read.1 read.2
    (resolve_columns_checked schema session root query h role read member) role

theorem resolve_tables (schema : Schema) (session : Session) (root : Selection)
    (query : Query) (h : resolve schema session root = .ok query) (role : session.role ≠ "admin") :
    ∀ scan ∈ allScans query, ∃ permission ∈ schema.permissions,
      permission.role = session.role ∧ permission.target = scan.name := by
  intro scan member
  have granted := resolve_tables_checked schema session root query h role scan member
  cases found : findPermission schema session.role scan.name with
  | none => simp [found] at granted
  | some permission =>
    have names := List.find?_some found
    exact ⟨permission, List.mem_of_find?_eq_some found, by simpa [Bool.and_eq_true] using names⟩

end model
