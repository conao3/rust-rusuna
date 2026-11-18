# rusuna

A Hasura-style GraphQL engine as a Rust library. rusuna compiles a GraphQL query over
tables and relationships into one PostgreSQL statement that returns the response as a
single JSON value. It runs inside your own process: no extra server, no database
extension.

The compiler core is written in Lean 4 with machine-checked theorems and is translated to
Rust by [leanfran](https://github.com/conao3/lean-leanfran).

## Status

rusuna loads a mapping schema from SDL and compiles GraphQL query documents to SQL.

| Feature | State |
|---|---|
| Lists, relationships, recursive filters, pagination, null ordering, aggregates and row `__typename` | available |
| SDL with directives, typed GraphQL text parsing, expanded SDL and a CLI | available |
| Row and column read permissions per role, with plan invariants | available |
| Mutations in one transaction | planned |
| Parts for an `async-graphql` schema and `axum` routes | planned |
| Subscriptions, event triggers | planned |

## Usage

```rust
use rusuna::{Schema, Session};
use serde_json::json;

let schema = Schema::from_sdl(r#"
    type Author @table(name: "author") {
        id: Int!
        name: String!
    }
    type Article @table(name: "article") {
        id: Int!
        title: String!
        author: Author! @relation(columns: ["author_id"], references: ["id"])
    }
    type Query { articles: [Article!]! @list }
"#)?;
let fields = schema.compile(
    "query Articles($limit: Int = 20) { articles(limit: $limit) { id title author { name } } }",
    &json!({}),
    Some("Articles"),
    &Session::new("admin"),
)?;
for field in fields {
    println!("{}: {} {:?}", field.response_key, field.compiled.sql, field.compiled.params);
}
```

Execute each `CompiledField.compiled` with a PostgreSQL client and put its JSON result
under `CompiledField.response_key` in the response's `data` object. Compilation performs
no I/O. `Schema::parse_query(query, &variables, operation_name, role)` returns the expanded
`Vec<Selection>`; `Schema::compile` additionally calls the Lean compiler for each root.
`variables` is a JSON object; `operation_name: Option<&str>` is required when a document
has multiple operations. `Error::Document { message, line, column }` reports syntax and
type errors with one-based positions. The existing `Error` variants report Lean
selection errors.

## Mapping SDL

```graphql
scalar DateTime

type Author @table(name: "author", schema: "public") {
  id: Int!
  name: String!
}
type Comment @table(name: "comment") {
  id: Int!
  body: String!
}
type Article @table(name: "article") {
  id: Int!
  title: String!
  publishedAt: DateTime! @column(name: "published_at")
  author: Author! @relation(columns: ["author_id"], references: ["id"])
  comments: [Comment!]! @relation(columns: ["id"], references: ["article_id"])
  commentsAggregate: Comment_aggregate! @aggregate(target: "Comment") @relation(columns: ["id"], references: ["article_id"])
}
type Query {
  articles: [Article!]! @list
  articleStats: Article_aggregate! @aggregate(target: "Article")
}
```

`@table(name: String!, schema: String)` maps a row type to a SQL table; `schema` defaults
to `public`. Scalar fields use their own names as SQL columns, or
`@column(name: String!)`. `@relation(columns: [String!]!, references: [String!]!)` uses
SQL column names, including columns hidden from GraphQL. A list type selects an array
relationship; a named row type selects an object relationship. Both key lists must be
nonempty, distinct and equally long.

`@list` declares a list root. `@aggregate(target: String!)` requires the generated
non-list type `<target>_aggregate` and can be combined with `@relation` on row fields.
The generated aggregate types are referenced but not declared in the mapping SDL.
A `schema { query: Root }` definition selects a custom query root type.

`Schema::expanded_sdl(role)` returns deterministic SDL with `where`, `order_by`, `limit`
and `offset` arguments on list and aggregate fields. Object relationships have no
arguments. The output includes `<Type>_bool_exp`, `<Type>_order_by`, scalar comparison
inputs, `order_by`, and aggregate output types. This same expanded type graph validates
query values and aggregate operators. `tests/fixtures/schema.graphql` and
`tests/fixtures/schema.expanded.graphql` are checked for equality by `make check`.

Supported scalars are `Int` (signed 32-bit GraphQL input), `Float`, `String`, `Boolean`,
`ID` and the declared string-valued `DateTime`. The database is responsible for validating
DateTime syntax and SQL column compatibility. Table types need at least one scalar
column. The row field names `_and`, `_or` and `_not` are reserved for filter operators.
Interfaces, unions, enums, custom input definitions, extensions, custom scalar
coercion and custom directives are outside this mapping format.

Queries support variables and defaults, aliases, named and inline fragments,
`@skip` and `@include`, Hasura v2 filters and ordering (single object or list), pagination,
aggregates and row `__typename`. Repeated compatible fields are merged in first-appearance
order. Conflicting fields, fragment cycles, mutation, subscription and introspection
produce errors. `__typename` on the query root and generated aggregate containers is
unsupported. A row selection emptied by conditional directives is rejected by the Lean
selection rules; a skipped root emits no statement.

Documents and variables are bounded to 1 MiB, document nesting to 128 levels and fragment
expansion to 100,000 visited selections, before the core's tighter selection limits.
The parser is [async-graphql-parser](https://docs.rs/async-graphql-parser/7.2.1/async_graphql_parser/)
7.2.1 with async-graphql-value 7.2.1. Database execution belongs to the caller.

## Read permissions

```graphql
type Article @table(name: "article")
  @permission(role: "user", columns: ["id", "title"],
              where: {author_id: {_eq: "$user_id"}}, allow_aggregations: true) {
  id: Int!
  title: String!
  author_id: Int!
}
type Query {
  articles: [Article!]! @list
  articleStats: Article_aggregate! @aggregate(target: "Article")
}
```

`@permission(role: String!, columns: [String!]!, where: <Type>_bool_exp!,
allow_aggregations: Boolean)` is repeatable, once per role per table. Column names are
GraphQL field names; an empty list grants no scalar columns. `allow_aggregations`
defaults to false. Filters use the same grammar as query filters, including relationships.
A scalar string `"$name"` reads a session variable; `"$$price"` is the literal `"$price"`.
Names after `$` contain letters, digits or underscores. `_is_null` takes a literal
Boolean, since it selects SQL syntax rather than a bound value. Membership lists can contain
individual session references; a session variable cannot hold a list. Permission `limit`
is not supported: limiting selected nodes and aggregate inputs requires a separate
Hasura-compatible plan rule. Supplying that argument is an error.

```rust
let session = Session::new("user").variable("user_id", rusuna::Value::Int(3));
let fields = schema.compile("{ articles { id title } }", &json!({}), None, &session)?;
```

The caller constructs the role and session from its authentication result. rusuna does
not authenticate a request. `Session::from_json(role, &object)` accepts scalar JSON
values; missing referenced variables, null comparisons and nonfinite numbers are errors.
The text API additionally checks session types against the permission's scalar fields.
`admin` is reserved and unrestricted; it requires no permission definitions. Declaring
an `admin` permission in SDL is an error. Every other role sees only granted tables and
columns, including filters, ordering and aggregate operands. Hidden roots and relationships
produce the same unknown-field errors as absent fields. Join keys and columns used by
the permission itself need not be selectable. Object relationships become nullable in
that role's expanded SDL because their target row can be hidden.

Each user table scan receives its permission as a conjunct: roots, object and array
relationships, aggregates and `EXISTS`. User predicates are traversed in document order;
a scan's permission values follow its user predicate values, before child selections.
Both literal and session permission values become parameters through `Tok.param`.

A relationship inside a permission has an additional restriction. Hasura evaluates that
predicate without automatically applying the target table's permission. To preserve its
result while ensuring every scan contains its role condition, rusuna requires the nested
predicate to already equal that condition or contain it as an immediate `_and` child.
For example, if Article requires `author_id = $user_id`, a Comment permission of
`{article: {author_id: {_eq: "$user_id"}}}` is supported. A different predicate on
Article that omits the condition is rejected with `InvalidPermission`, even if it would
logically imply it. This check uses the resolved expression after session binding; it
never searches the SQL text or expands permission cycles.

The lower-level API uses `Table.permissions: Vec<Permission>` and
`Permission { role, filter: Filter<Operand>, columns, allow_aggregations }`, where
`Operand` is `Literal(Value)` or `Session(String)`. Permission filters have the same
32-edge, 1000-child and 1000-membership limits as query filters. No mutation permissions
are implemented.

## CLI

```sh
nix run . -- schema expand-sdl --role admin --schema file://tests/fixtures/schema.graphql --output text
nix run . -- query compile-sql --role admin --schema file://tests/fixtures/schema.graphql --query file://tests/fixtures/queries/a.graphql
nix run . -- query compile-sql --role user --session '{"user_id":3}' --schema file://tests/fixtures/permissions.graphql --query '{ article { id } }'
```

`rusuna-cli` installs the binary `rusuna`. Values beginning with `file://` are read from
files, including `file:///dev/stdin`. `query compile-sql` accepts `--operation-name` and
`--variables` and `--session` (JSON objects or files). Both commands require `--role`. Global options are `--output json|text|table`
(default `json`), `--debug`, `--no-debug` and `--version`. Each command level has `help`.
JSON keys are `Sdl`, or `Fields` containing `ResponseKey`, `Sql` and `Params`. Text
expansion is raw SDL; query text is tab-separated and table output adds column names.
Exit codes are 0 for success, 252 for command syntax or parameter errors, 254 for SDL or
query errors, and 255 for other failures. Diagnostics go to stderr.

## Schema and selection values

The lower-level `Schema::new(tables: Vec<Table>, roots: Vec<RootField>)` returns `Result<Schema, Error>` and declares root fields explicitly. `Schema::compile_selection(&Selection, &Session)` invokes the Lean core directly; typed text compilation requires `Schema::from_sdl`.
`RootField::new(field, target, RootKind::List | RootKind::Aggregate)` targets a table's
GraphQL type name. `Table::name` is the row's `__typename`; it is independent of the root
field and SQL table names. Each `Relation` declares its field, target, column mapping and
`Kind::Object`, `Kind::Array` or `Kind::Aggregate`. Declare an aggregate relationship as a
separate field, for example `articles_aggregate`.

A `Selection` has public `alias`, `name`, `filter: Option<Filter>`,
`order_by: Vec<Order>`, `limit: Option<u64>`, `offset: Option<u64>` and
`fields: Vec<Selection>`. Its builders are `alias`, `filter`, `order_by`, `limit`,
`offset` and `field`. `filter` sets the entire expression; combine predicates explicitly:

```rust
use rusuna::{Comparison, Filter, Selection, Value};

let query = Selection::new("article")
    .filter(Filter::And(vec![
        Filter::compare("id", Comparison::Gt, Value::Int(2)),
        Filter::Relation {
            field: "author".into(),
            filter: Box::new(Filter::eq("name", Value::Text("Ada".into()))),
        },
    ]))
    .field(Selection::new("id"));
```

The recursive filter variants, in document order, are:

```rust
pub enum Filter {
    Compare { field: String, op: Comparison, value: Value },
    In { field: String, negated: bool, values: Vec<Value> },
    IsNull { field: String, value: bool },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    Relation { field: String, filter: Box<Filter> },
}
```

`Comparison` is `Eq`, `Neq`, `Gt`, `Gte`, `Lt`, `Lte`, `Like`, `Nlike`, `Ilike` or
`Nilike`. `Direction` is `Asc`, `AscNullsFirst`, `AscNullsLast`, `Desc`,
`DescNullsFirst` or `DescNullsLast`; the defaults are nulls last for ascending and nulls
first for descending.

`Value` is `Int(i64)`, `Float(f64)`, `Text(String)`, `Bool(bool)` or `Null`. Lean stores
float bits as `UInt64`, preserving decidable equality and finite values exactly.
Comparisons reject null, including null membership elements, as in
[Hasura v2](https://hasura.io/docs/2.0/queries/postgres/filters/index/).
Nonfinite floats are rejected. Test nullness with `Filter::IsNull`. The lower-level schema has no column SQL types. SDL compilation checks values and aggregate operators against GraphQL types; callers of `compile_selection` supply compatible values themselves.

Aggregate roots and relationships use ordinary selections for
`aggregate { count sum { column } avg { column } min { column } max { column } }` and
`nodes { fields }`, including aliases and multiple selections of nodes. `count` takes no
arguments. Aggregate containers take the same filter, ordering, limit and offset as
lists; their `aggregate` and `nodes` children take no arguments.

Selections have at most `MAX_FIELDS` (1000) fields. Relations are bounded by `MAX_DEPTH`
(32); aggregate selection wrappers also count toward nesting, with three extra wrapper
levels allowed. Filters have at most `MAX_FILTER_DEPTH` (32) edges from the root,
1000 children per boolean expression and `MAX_IN_VALUES` (1000) membership elements.
Rust bounds recursive conversion before entering Lean, and Lean checks the limits too.
Tests exercise the accepted boundaries on a 2 MiB stack, including 32 relationship
levels plus 32 `EXISTS` levels and a maximum-depth 1000-term permission (65,001 parameters).

## The generated SQL

- One statement per root field. PostgreSQL builds the JSON with `row_to_json` and
  `json_agg`.
- An object relationship and an array relationship with a `limit` or `offset` are read by a
  `LEFT JOIN LATERAL` subquery per parent row.
- An array relationship without a `limit` or `offset` whose parents are read as a set is read once:
  its rows are restricted to the parent rows with `IN`, aggregated by the parent key, and
  joined to the parents.
- Comparison values are statement parameters, including each element of `_in` and `_nin`.
  `limit` and `offset` are non-negative integers written into the statement. `_is_null`
  selects `IS NULL` or `IS NOT NULL`. Empty `_in` is false and empty `_nin` is true.
- Every identifier is double-quoted, with the quotes inside it doubled. Row type names
  are single-quoted literals with doubled quotes; use PostgreSQL
  `standard_conforming_strings = on`.
- Relationship predicates become correlated `EXISTS` subqueries, including all columns
  of a composite relationship. Empty `_and` is true and empty `_or` is false.
- Aggregates share the filtered, ordered and paginated row set with `nodes`. Only the
  selected aggregate functions are emitted. Empty sets have count 0, null metrics and
  empty nodes. Unlimited aggregate relationships use the same parent-set restriction
  and grouping as list relationships.

`tests/fixtures/expected/` holds the statements for the queries of
`tests/fixtures/queries/`.

## What is proved

The Lean definitions and theorems are in `src/compiler.rs`. Each theorem is universally quantified over its Lean inputs.
[PROOFS.md](PROOFS.md) lists the exact statements and their classification.
The permission statements concern successful resolution with a schema and session.

Properties:

| Theorem | Statement |
|---|---|
| `run_params`, `compile_params`, `resolve_values` | Parameters follow document order, including each scan's bound permission values after its user filter. |
| `compile_erase` | Resolved plans that differ only in their query or session-derived values produce the same SQL text. |
| `placeholders_numbered`, `slots_numbered` | The placeholders are `$1` … `$n`, in the order of the parameters. |
| `unquote_quote`, `unquote_quoteChars` | `unquoteChars`, a reader of a double-quoted identifier written after PostgreSQL's lexical rules, reads a quoted name back as the name and stops at its closing quote. |
| `unquote_quoteString`, `unquote_quoteStringChars` | Reading a quoted string literal returns the original string and stops at the closing quote. |
| `resolve_declared` | Every table in the plan, including relationship predicates inside `EXISTS`, is declared in the schema. |
| `distinct_iff` | The check for repeated aliases accepts exactly the lists without duplicates. |
| `resolve_scan_conditions`, `resolve_tables` | Every non-admin scan, including scans inside permission predicates, contains the bound condition of that role and has a permission definition. |
| `resolve_columns` | User column references in selections, filters, ordering and aggregates belong to the role's visible columns; join keys and policy columns are excluded. |

`compile_erase` erases `Value` operands, including bound session operands. It does not
erase pagination, `_is_null` flags, names, role choices or permission syntax.

Conditions written a second time, as an equivalence next to the definition:

| Theorem | Statement |
|---|---|
| `aliasValid_iff`, `selectionValid_iff` | An accepted selection is not empty, has at most 1000 fields, and its aliases are distinct, not empty, not `%` and at most 63 bytes long. |
| `relationValid_iff` | An accepted relationship has as many columns as references, and at least one. |
| `lateralScan_object`, `lateralScan_array`, `lateralScan_aggregate`, `lateralScan_limit` | The scan of an object relationship has the limit 1 and the conditions of the relationship; array and aggregate scans are unchanged. |
| `scalarValid_iff`, `membershipValid_iff` | Comparison values are non-null and finite, and membership lists respect their size bound. |
| `predicateEq_iff`, `predicatesEq_iff`, `hasConjunct_iff` | Structural expression equality and immediate conjunct membership have their stated meanings. |
| `appendCondition_exact`, `addPermission_condition` | The inserted scan is exactly the original scan with the requested role condition appended; binding errors are preserved. |

[leanassay](https://github.com/conao3/lean-leanassay/tree/fc0d05473917864cedb6d27e208c81de3e2a11af) checks which theorems rule out
broken implementations: the audit covers fourteen functions, with 101 mutations, 78 killed,
16 equivalent on the sampled inputs, 7 stillborn, zero holes, zero untested and zero timed out.
There are nine independent properties (fifteen principal theorem statements), fourteen
restated conditions and zero variable-free checks. The executable audit checks full
scan equalities as well as their limit projection. AGENTS.md lists the functions it covers and the ones it does not.

The permission insertion audit has the following table (`✗` means a theorem refuted
the mutation):

| Mutation | `appendCondition_exact` | `addPermission_condition` |
|---|---|---|
| `appendCondition := fun _ _ => default` | ✗ | ✗ |
| `appendCondition := fun x _ => x` | ✗ | ✗ |
| `addPermission := fun _ _ _ => default` | — | ✗ |

Replacing the requested role with `"other"` is not generated by the pinned mutator.
A manual mutation failed both the insertion and parameter-preservation proofs.

`make survey` examines functions outside that audit with the boundary theorems:

| Group | Functions | Mutations | Killed | Holes | Equivalent on sampled inputs | Untested | Timed out | Stillborn |
|---|---|---|---|---|---|---|---|---|
| SQL emission | 8 | 176 | 23 | 100 | 2 | 0 | 0 | 51 |
| Selection resolution | 9 | 178 | 5 | 50 | 17 | 0 | 26 | 80 |
| Permission resolution | 3 | 1 | 0 | 1 | 0 | 0 | 0 | 0 |
| Total | 20 | 355 | 28 | 151 | 19 | 0 | 26 | 131 |

The survey uses 1000 samples and a 30-second deadline per mutation. Timeouts are
unresolved and depend on machine load. Equivalent means equal outputs on the sampled
inputs; untested means an output comparison could not be performed. Stillborn
mutations do not typecheck. The holes concern SQL text and acceptance rules outside
the properties proved above; the tests cover these by example.

Outside the proofs:

- What PostgreSQL returns. The row permission theorem proves that each `Scan` contains
  its role condition as an expression in the plan. It does not interpret SQL execution,
  or prove that the expression represents the application author's intended policy.
  Differential tests compare PostgreSQL results with Hasura-generated statements.
- The complete acceptance rules of `resolveSelection` and `resolve`. An independent
  Bool acceptance function and its success equivalence remain pending; no theorem
  fixes the complete accepted set or every error variant. Tests cover them by example.
- leanfran, which translates the Lean definitions to the Rust that runs. The tests run
  the generated Rust.
- `src/lib.rs`, which converts public Rust types to Lean types and back; SDL parsing,
  role-specific SDL rendering and scalar type checking in `src/graphql/`; authentication
  and construction of the session by the caller.
- `unquoteChars` as a description of PostgreSQL's reader.

## Results and speed

`make check` compares, on a small data set, the result of every statement rusuna generates
for `tests/fixtures/queries/` with the result of the statement Hasura v2.51.6 generates
for the same query (`tests/fixtures/hasura/`).

`make bench` loads 10,000 authors, 200,000 articles and 1,000,000 comments and runs, on
one connection, the statement of rusuna, of [viat-gql](https://crates.io/crates/viat-gql)
1.0.1, of Hasura v2.51.6 and a hand-written one in turn, 15 times each. Milliseconds on a
6-core x86-64 Linux machine with PostgreSQL 17.12, median (minimum to maximum):

| Query | rusuna | viat-gql | Hasura | hand-written |
|---|---|---|---|---|
| A: 20 articles, their author and 5 comments each | 0.7 (0.6–7.4) | 0.6 (0.6–3.9) | 0.7 (0.6–7.1) | 1.1 (0.5–7.6) |
| B: 50,000 articles and their author | 426.5 (125.8–580.3) | 427.3 (315.8–798.4) | 415.3 (130.1–565.8) | 504.8 (222.9–879.5) |
| C: 2,000 authors, all their articles and all their comments | 1269.3 (743.5–2340.7) | 1257.0 (772.8–2514.4) | 1610.2 (734.3–2128.6) | 1336.9 (901.6–2038.7) |

All three speed goals pass in this run. Compiling these GraphQL documents, including parsing and type checking, takes 274 to 506 µs.
All four implementations return equal results on the benchmark data. The differential
test covers 43 admin queries and 19 permission queries, including empty results, nested
aggregates, custom root names, nullable columns, composite relationship predicates and
every supported comparison. Ten rejected role queries are fixtures too. The permission
fixtures use two roles and session IDs 1 and 3, covering roots, object and array
relationships, set and lateral aggregation, and nested `EXISTS`. Each captured Hasura
statement was also compared with its role-bearing HTTP response. The exported metadata
is in `tests/fixtures/permissions/metadata.json`.

With role `user` and `user_id = 3`, the same benchmark measures A at 1.0 ms (0.7–3.9),
B at 0.3 ms (0.2–7.8), and C at 7.5 ms (3.6–10.0), 15 runs each. These smaller result
sets also match Hasura. The timings include concurrent machine load; compare the
statements within the same run.

## Development

```
nix develop --command make check
nix build
```

`make check` runs the format check, Clippy, the Lean proofs (`lake -d lean build --wfail`),
the theorem audit (`lake -d audit build --wfail`), the check that the generated files match
the source, and the tests against a throwaway PostgreSQL 17 cluster. `make bench` runs the
measurement above, `make survey` prints the leanassay tables of the functions outside the
audit. Enable the optional pre-commit hook with `git config core.hooksPath .githooks`.

## License

Apache-2.0
