leanfran::lean! {
    /// The compiler core: SQL tokens, the query plan and its resolution against a schema.
    pub mod model;
    r#"
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

    inductive Expression (R : Type) where
      | compare (field : String) (op : Comparison) (value : Value)
      | member (field : String) (negated : Bool) (values : List Value)
      | isNull (field : String) (value : Bool)
      | and (children : List (Expression R))
      | or (children : List (Expression R))
      | not (child : Expression R)
      | relation (reference : R) (child : Expression R)
      deriving Repr

    mutual
    def expressionValues {R : Type} : Expression R → List Value
      | .compare _ _ v => [v]
      | .member _ _ vs => vs
      | .isNull _ _ => []
      | .and es | .or es => expressionsValues es
      | .not e | .relation _ e => expressionValues e
    def expressionsValues {R : Type} : List (Expression R) → List Value
      | [] => []
      | e :: es => expressionValues e ++ expressionsValues es
    end

    mutual
    def eraseExpression {R : Type} : Expression R → Expression R
      | .compare f op _ => .compare f op (.int 0)
      | .member f neg vs => .member f neg (vs.map fun _ => .int 0)
      | .isNull f b => .isNull f b
      | .and es => .and (eraseExpressions es)
      | .or es => .or (eraseExpressions es)
      | .not e => .not (eraseExpression e)
      | .relation r e => .relation r (eraseExpression e)
    def eraseExpressions {R : Type} : List (Expression R) → List (Expression R)
      | [] => []
      | e :: es => eraseExpression e :: eraseExpressions es
    end

    def comparisonText : Comparison → String
      | .eq => "=" | .neq => "<>" | .gt => ">" | .gte => ">=" | .lt => "<" | .lte => "<="
      | .like => "LIKE" | .nlike => "NOT LIKE" | .ilike => "ILIKE" | .nilike => "NOT ILIKE"

    def paramToks : List Value → List Tok
      | [] => []
      | v :: vs => .param v :: vs.flatMap (fun x => [.lex (.raw ","), .param x])

    mutual
    def expressionToks (depth : Nat) (qualifier : String) : Expression Reference → List Tok
      | .compare field op v => lift [.col qualifier field, .raw (comparisonText op)] ++ [.param v]
      | .member field neg vs =>
        if vs.isEmpty then lift [.raw (if neg then "true" else "false")]
        else lift [.col qualifier field, .raw (if neg then "NOT IN (" else "IN (")]
          ++ paramToks vs ++ lift [.raw ")"]
      | .isNull field b => lift [.col qualifier field, .raw (if b then "IS NULL" else "IS NOT NULL")]
      | .and es => lift [.raw "( true"] ++ expressionsToks depth qualifier "AND" es ++ lift [.raw ")"]
      | .or es => lift [.raw "( false"] ++ expressionsToks depth qualifier "OR" es ++ lift [.raw ")"]
      | .not e => lift [.raw "NOT ("] ++ expressionToks depth qualifier e ++ lift [.raw ")"]
      | .relation ref e =>
        let alias := "e" ++ toString depth
        lift ([.raw "EXISTS (SELECT 1 FROM", .col ref.schema ref.table, .raw "AS", .ident alias,
          .raw "WHERE true"] ++ (ref.parent.zip ref.child).flatMap (fun pair =>
            [.raw "AND", .col alias pair.2, .raw "=", .col qualifier pair.1]) ++ [.raw "AND ("])
          ++ expressionToks (depth + 1) alias e ++ lift [.raw "))"]
    def expressionsToks (depth : Nat) (qualifier op : String) : List (Expression Reference) → List Tok
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
    theorem expressionToks_values (depth : Nat) (q : String) (e : Expression Reference) :
        (expressionToks depth q e).filterMap tokValue = expressionValues e := by
      cases e <;> simp only [expressionToks, expressionValues]
      all_goals try split
      all_goals simp_all [lift_values, paramToks_values, tokValue, List.filterMap, expressionToks_values, expressionsToks_values]
    theorem expressionsToks_values (depth : Nat) (q op : String) (es : List (Expression Reference)) :
        (expressionsToks depth q op es).filterMap tokValue = expressionsValues es := by
      cases es <;> simp [expressionsToks, expressionsValues, lift_values, expressionToks_values, expressionsToks_values]
    end

    mutual
    theorem expressionToks_erase (depth : Nat) (q : String) (e : Expression Reference) :
        (expressionToks depth q e).map eraseTok = expressionToks depth q (eraseExpression e) := by
      cases e <;> simp only [expressionToks, eraseExpression]
      all_goals try split
      all_goals simp_all [lift_erase, paramToks_erase, eraseTok, expressionToks_erase, expressionsToks_erase]
    theorem expressionsToks_erase (depth : Nat) (q op : String) (es : List (Expression Reference)) :
        (expressionsToks depth q op es).map eraseTok = expressionsToks depth q op (eraseExpressions es) := by
      cases es <;> simp [expressionsToks, eraseExpressions, lift_erase, expressionToks_erase, expressionsToks_erase]
    end

    structure Scan where
      schema : String
      table : String
      conds : Expression Reference
      order : List (String × Direction)
      limit : Option Nat
      offset : Option Nat
      deriving Repr

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
      | .object => { scan with order := [], limit := some 1, offset := none }
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

    def scanValues (scan : Scan) : List Value := expressionValues scan.conds

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

    def eraseScan (scan : Scan) : Scan := { scan with conds := eraseExpression scan.conds }

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
      simp [scanToks, eraseScan, lift_erase, expressionToks_erase]

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
      | relation alias kind scan parent child children => simp [eraseNode, fieldLex, eraseScan, emptyRow_erase]
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
      simp [resultLex, rowLex_erase, eraseScan]

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
        lateralScan .object scan = { scan with order := [], limit := some 1, offset := none } := rfl

    theorem lateralScan_limit (kind : Kind) (scan : Scan) :
        (lateralScan kind scan).limit = (if kind = .object then some 1 else scan.limit) := by
      cases kind <;> rfl

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

    structure Schema where
      tables : List Table
      roots : List RootField
      deriving DecidableEq, Repr

    structure Order where
      field : String
      direction : Direction
      deriving DecidableEq, Repr

    structure Selection where
      alias : String
      name : String
      conds : Option (Expression String)
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
      | tooDeep
      deriving DecidableEq, Repr

    def maxDepth : Nat := 32

    def maxFields : Nat := 1000

    def maxFilterDepth : Nat := 32

    def maxInValues : Nat := 1000

    def filterValues (f : Option (Expression String)) : List Value :=
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
        (e : Expression String) : Except Error (Expression Reference) :=
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
          else (resolveExpressions schema table (depth + 1) es).map Expression.and
        | .or es =>
          if maxFields < es.length then .error .invalidFilter
          else (resolveExpressions schema table (depth + 1) es).map Expression.or
        | .not e => (resolveExpression schema table (depth + 1) e).map Expression.not
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
                .relation ⟨target.schema, target.table, relation.parent, relation.child⟩ out
    termination_by sizeOf e
    def resolveExpressions (schema : Schema) (table : Table) (depth : Nat)
        (es : List (Expression String)) : Except Error (List (Expression Reference)) :=
      match es with
      | [] => .ok []
      | e :: es =>
        match resolveExpression schema table depth e with
        | .error err => .error err
        | .ok out => (resolveExpressions schema table depth es).map (out :: ·)
    termination_by sizeOf es
    end

    def resolveFilter (schema : Schema) (table : Table) :
        Option (Expression String) → Except Error (Expression Reference)
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

    def resolveScan (schema : Schema) (table : Table) (conds : Option (Expression String)) (order : List Order) (limit offset : Option Nat) :
        Except Error Scan :=
      match resolveFilter schema table conds with
      | .error e => .error e
      | .ok conds =>
        match resolveOrder table order with
        | .error e => .error e
        | .ok order => .ok { schema := table.schema, table := table.table, conds, order, limit, offset }

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

    def resolve (schema : Schema) (root : Selection) : Except Error Query :=
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

    def run (schema : Schema) (root : Selection) : Except Error Compiled :=
      match resolve schema root with
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
        (e : Expression String) (out : Expression Reference)
        (h : resolveExpression schema table depth e = .ok out) :
        expressionValues out = expressionValues e := by
      cases e <;> simp only [resolveExpression] at h
      all_goals repeat' first | cases h | split at h
      all_goals try simp only [Except.map] at h
      all_goals repeat' first | cases h | split at h
      all_goals simp only [expressionValues]
      all_goals first
        | rfl
        | exact resolveExpression_values _ _ _ _ _ (by assumption)
        | exact resolveExpressions_values _ _ _ _ _ (by assumption)
    theorem resolveExpressions_values (schema : Schema) (table : Table) (depth : Nat)
        (es : List (Expression String)) (out : List (Expression Reference))
        (h : resolveExpressions schema table depth es = .ok out) :
        expressionsValues out = expressionsValues es := by
      cases es <;> simp only [resolveExpressions] at h
      all_goals repeat' first | cases h | split at h
      all_goals try simp only [Except.map] at h
      all_goals repeat' first | cases h | split at h
      all_goals simp only [expressionsValues]
      all_goals first
        | rfl
        | rw [resolveExpression_values _ _ _ _ _ (by assumption),
            resolveExpressions_values _ _ _ _ _ (by assumption)]
    end

    theorem resolveFilter_values (schema : Schema) (table : Table) (f : Option (Expression String))
        (out : Expression Reference) (h : resolveFilter schema table f = .ok out) :
        expressionValues out = filterValues f := by
      cases f with
      | none => cases h; rfl
      | some e => simpa [resolveFilter, filterValues] using resolveExpression_values schema table 0 e out h

    theorem resolveScan_values (schema : Schema) (table : Table) (conds : Option (Expression String)) (order : List Order)
        (limit offset : Option Nat) (scan : Scan) (h : resolveScan schema table conds order limit offset = .ok scan) :
        scanValues scan = filterValues conds := by
      simp only [resolveScan] at h
      split at h
      · cases h
      · rename_i out hout
        split at h
        · cases h
        · cases h
          simpa [scanValues] using resolveFilter_values schema table conds out hout

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

    theorem resolve_values (schema : Schema) (root : Selection) :
        match resolve schema root with
        | .ok query => queryValues query = selectionValues root
        | .error _ => True := by
      split
      · rename_i query h
        simp only [resolve] at h
        repeat' first | cases h | split at h
        simp only [queryValues]
        rw [resolveScan_values _ _ _ _ _ _ _ (by assumption),
          resolveNodes_values _ _ _ _ _ _ _ (by assumption)]
        cases root; rfl
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

    def referenceDeclared (schema : Schema) (ref : Reference) : Bool :=
      schema.tables.any fun t => t.schema = ref.schema && t.table = ref.table

    mutual
    def expressionDeclared (schema : Schema) : Expression Reference → Bool
      | .compare _ _ _ | .member _ _ _ | .isNull _ _ => true
      | .and es | .or es => expressionsDeclared schema es
      | .not e => expressionDeclared schema e
      | .relation ref e => referenceDeclared schema ref && expressionDeclared schema e
    def expressionsDeclared (schema : Schema) : List (Expression Reference) → Bool
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
        (e : Expression String) (out : Expression Reference)
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
        (es : List (Expression String)) (out : List (Expression Reference))
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

    theorem resolveFilter_declared (schema : Schema) (table : Table) (f : Option (Expression String))
        (out : Expression Reference) (h : resolveFilter schema table f = .ok out) :
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
        (conds : Option (Expression String)) (order : List Order) (limit offset : Option Nat) (scan : Scan)
        (hfind : findTable schema name = some table)
        (hscan : resolveScan schema table conds order limit offset = .ok scan) : declared schema scan = true := by
      simp only [resolveScan] at hscan
      split at hscan
      · cases hscan
      · rename_i out hout
        split at hscan
        · cases hscan
        · cases hscan
          simp only [declared, Bool.and_eq_true]
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

    theorem resolve_declared (schema : Schema) (root : Selection) :
        match resolve schema root with
        | .ok query => (queryScans query).all (declared schema) = true
        | .error _ => True := by
      split
      · rename_i query h
        simp only [resolve] at h
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
    "#
}
