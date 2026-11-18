# rusuna

A Hasura-style GraphQL engine as a Rust library. rusuna compiles a GraphQL query or mutation over
tables and relationships into one PostgreSQL statement that returns the response as a
single JSON value. It runs inside your own process: no extra server, no database
extension.

The compiler core is written in Lean 4 with machine-checked theorems and is translated to
Rust by [leanfran](https://github.com/conao3/lean-leanfran).

## Status

rusuna loads a mapping schema from SDL and compiles GraphQL query, mutation and subscription
documents to SQL. `rusuna-events` runs event triggers and cron triggers beside it.

| Feature | State |
|---|---|
| Lists, relationships, recursive filters, pagination, null ordering, aggregates and row `__typename` | available |
| SDL with directives, typed GraphQL text parsing, expanded SDL and a CLI | available |
| Row and column read permissions per role, with plan invariants | available |
| Row limits of read permissions on lists and aggregate `nodes` | available |
| Insert, update, delete, primary-key roots and write permissions | available |
| Views and materialized views as read-only tables | available |
| Native queries: SELECT statements in SDL as root fields with arguments | available |
| Execution with sqlx, parts for an `async-graphql` schema and `axum` routes | available |
| Subscriptions over WebSocket, polled with one statement per group of subscriptions | available |
| REST endpoints from named GraphQL documents | available |
| Remote schemas: root fields of other GraphQL servers on the same endpoint | available |
| Event triggers, cron triggers and scheduled events, delivered to Rust handlers and webhooks | available |

The workspace has these crates:

| Crate | Role |
|---|---|
| `rusuna` | The compiler: SDL, GraphQL text and Lean core. No I/O. |
| `rusuna-sqlx` | Runs compiled statements on PostgreSQL with sqlx 0.9 and returns the `data` JSON; polls subscriptions. |
| `rusuna-async-graphql` | Builds an async-graphql 7 dynamic schema for one role, beside hand-written fields and remote schemas. |
| `rusuna-axum` | axum 0.8 routes (`POST`, WebSocket and REST endpoints) that pick the role's schema and execute the request. |
| `rusuna-events` | Records row changes with PostgreSQL triggers and delivers them, cron occurrences and scheduled events to Rust handlers and webhooks. |
| `rusuna-http-client` | The hyper 1 and rustls HTTP client of webhooks and remote schemas. |
| `rusuna-cli` | The `rusuna` command (not published). |

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
under `CompiledField.response_key` in the response's `data` object; `rusuna-sqlx` does
this. `CompiledField.mutation` is true for mutation roots. Compilation performs no I/O. `Schema::parse_query(query, &variables, operation_name, role)` returns the expanded
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

`@table(name: String!, schema: String, read_only: Boolean)` maps a row type to a SQL
table, view or materialized view; `schema` defaults to `public`. `read_only: true` (default
false) removes the type's mutation roots and their input types for every role, `admin`
included; a write permission on such a type is an error (next sections). Scalar fields use their own names as SQL columns, or
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
inputs, `order_by`, and aggregate output types. The `Subscription` root repeats the fields of
the query root. This same expanded type graph validates
query values and aggregate operators. `tests/fixtures/schema.graphql` and
`tests/fixtures/schema.expanded.graphql` are checked for equality by `make check`.

Supported scalars are `Int` (signed 32-bit GraphQL input), `Float`, `String`, `Boolean`,
`ID` and the declared string-valued `DateTime`. The database is responsible for validating
DateTime syntax and SQL column compatibility. Table types need at least one scalar
column. The row field names `_and`, `_or` and `_not` are reserved for filter operators.
The generated root types `Mutation` and `Subscription` cannot share a name with a
declared type. Interfaces, unions, enums, custom input definitions, extensions, custom scalar
coercion and custom directives are outside this mapping format.

Queries support variables and defaults, aliases, named and inline fragments,
`@skip` and `@include`, Hasura v2 filters and ordering (single object or list), pagination,
aggregates and row `__typename`. Repeated compatible fields are merged in first-appearance
order. Conflicting fields, fragment cycles and introspection produce errors. A subscription
operation selects exactly one root field of the query root and compiles to the statement of
the same query. `__typename` on the query root and generated aggregate containers is
unsupported. A row selection emptied by conditional directives is rejected by the Lean
selection rules; a skipped root emits no statement.

Documents and variables are bounded to 1 MiB, document nesting to 128 levels and fragment
expansion to 100,000 visited selections, before the core's tighter selection limits.
The parser is [async-graphql-parser](https://docs.rs/async-graphql-parser/7.2.1/async_graphql_parser/)
7.2.1 with async-graphql-value 7.2.1. `Schema::roles()` lists `admin` and every role
named by a read or write permission.

### Views and materialized views

A view or a materialized view is mapped like a table, and reads it the same way:
relationships in both directions, filters, ordering, aggregates, read permissions and
subscriptions. rusuna does not read the database catalog, so it does not know that a
relation is a view; declare `read_only: true` for one that is not updatable, which every
materialized view is. A simple view that PostgreSQL can update may stay writable.

```graphql
type author_stats @table(name: "author_stats", read_only: true) @primary_key(columns: ["author_id"])
  @permission(role: "user", columns: ["author_id", "articles"], where: {}) {
  author_id: Int!
  articles: Int!
  author: author! @relation(columns: ["author_id"], references: ["id"])
}
```

- A view has no primary key in PostgreSQL. Without `@primary_key` it has no `<t>_by_pk`
  root; with one, it has the `<t>_by_pk` query and subscription root, which reads the row
  whose declared columns equal the arguments. The declaration is the caller's promise that
  these columns are unique.
- A read-only type has no `insert_<t>`, `update_<t>`, `delete_<t>` or `_by_pk` mutation
  roots and none of their input types (`<t>_insert_input`, `<t>_on_conflict`, …).
  `Schema::compile_mutation` on it returns `Error::ReadOnly`.
- A subscription to a materialized view sends a new result after `REFRESH MATERIALIZED
  VIEW` changes it; changes of its base tables do not reach it before.

The lower-level API sets `Table::read_only` (`Table::new(…).read_only()`).

### Native queries

A type declared with `@native_query` reads its rows from a SELECT statement written in the
SDL, like Hasura's native queries:

```graphql
type article_search @native_query(
  sql: "SELECT a.id, a.author_id, a.title FROM article AS a WHERE a.id >= {{min_id}} AND a.title LIKE {{pattern}}",
  arguments: {min_id: "Int!", pattern: "String"})
  @permission(role: "user", columns: ["id", "title"], where: {author_id: {_eq: "$user_id"}}) {
  id: Int!
  author_id: Int!
  title: String!
  author: author! @relation(columns: ["author_id"], references: ["id"])
}
type author_stats @native_query(sql: "SELECT author_id, count(*)::int AS articles FROM article GROUP BY author_id") {
  author_id: Int!
  articles: Int!
}
type Query {
  article_search: [article_search!]! @list
  article_search_aggregate: article_search_aggregate! @aggregate(target: "article_search")
}
```

```graphql
{ article_search(min_id: 3, pattern: "title%", where: {id: {_lt: 10}}, order_by: {id: desc}, limit: 5) { id title author { name } } }
```

- `@native_query(sql: String!, arguments: {name: "Scalar" | "Scalar!", …})` replaces
  `@table`; the type takes no `@primary_key`. Columns map to the statement's result columns.
  The type is read-only: it has no mutation roots, and write permissions on it are errors.
- `{{name}}` in the statement stands for the argument `name`. Each occurrence becomes a
  statement parameter `$n`, in the order of the occurrences; no value is written into the SQL
  text. PostgreSQL infers the type of each parameter from its context; write a cast
  (`{{since}}::timestamptz`) where the context does not decide it.
- The list and aggregate root fields of the type take the declared arguments before `where`,
  `order_by`, `limit` and `offset`; a non-null argument is required and an omitted nullable one
  is `null`. Argument names are GraphQL names other than `where`, `order_by`, `limit` and
  `offset`.
- The statement is embedded as `FROM (<statement>) AS "s"`, so filters, ordering, pagination,
  aggregates, relationships, read permissions (row conditions, columns and `limit`),
  subscriptions, the async-graphql schema and REST endpoints work as on a table.
- Relationships from a native query type to tables and to other native query types work in
  every direction rusuna supports for tables. A relationship to a native query type, and a
  relationship predicate in `where` or in a permission, need a type without arguments:
  arguments are given on root fields only, and the SDL rejects a relationship to a type with
  arguments.
- The statement is trusted input of the schema author, like the rest of the SDL. rusuna does
  not parse it beyond its quotes: it must be one statement without `;`, `$`, comments or
  backslashes, with `{{` only outside quoted text, so that the subscription multiplexer can
  read the placeholders of the compiled statement.

The lower-level API builds such a type with `Table::statement(name, sql, arguments)`, which
sets `Table::body` to `Body::Statement { parts, arguments }`; a `Selection` passes the root
arguments with `Selection::argument(name, value)` in the declared order.

## Read permissions

```graphql
type Article @table(name: "article")
  @permission(role: "user", columns: ["id", "title"],
              where: {author_id: {_eq: "$user_id"}}, allow_aggregations: true, limit: 20) {
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
allow_aggregations: Boolean, limit: Int)` is repeatable, once per role per table. Column names are
GraphQL field names; an empty list grants no scalar columns. `allow_aggregations`
defaults to false. Filters use the same grammar as query filters, including relationships.
A scalar string `"$name"` reads a session variable; `"$$price"` is the literal `"$price"`.
Names after `$` contain letters, digits or underscores. `_is_null` takes a literal
Boolean, since it selects SQL syntax rather than a bound value. Membership lists can contain
individual session references; a session variable cannot hold a list.

`limit` (a non-negative integer, 0 included) bounds the rows the role reads, as in Hasura
v2.51.6:

- A list root and an array relationship read at most `limit` rows: the query's own `limit`
  when it is smaller, `limit` otherwise. The query's `offset` applies before the bound.
- An aggregate root or aggregate relationship computes `aggregate` over the rows the query
  selects, with the query's `limit` and `offset` only. Its `nodes` list the first `limit` of
  those rows in the query's order.
- The `<t>_by_pk` root, object relationships, relationship predicates in `where` (`EXISTS`)
  and the top-level `returning` rows of a mutation are not bounded. Array relationships and
  aggregate `nodes` inside `returning` are.

Which rows remain without `order_by` is up to PostgreSQL, as in Hasura.

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
`Permission { role, filter: Filter<Operand>, columns, allow_aggregations, limit }`, where
`Operand` is `Literal(Value)` or `Session(String)`. Permission filters have the same
32-edge, 1000-child and 1000-membership limits as query filters. Write permissions are
`Table::write_permissions` (next section).

## Mutations and primary keys

```graphql
type article @table(name: "article") @primary_key(columns: ["id"])
  @permission(role: "user", columns: ["id", "author_id", "title", "score"], where: {})
  @insert_permission(role: "user", columns: ["id", "title", "score"],
    check: {score: {_gte: 0}}, set: {author_id: "$user_id"})
  @update_permission(role: "user", columns: ["title", "score"],
    where: {author_id: {_eq: "$user_id"}}, check: {score: {_gte: 0}})
  @delete_permission(role: "user", where: {author_id: {_eq: "$user_id"}}) {
  id: Int!
  author_id: Int!
  title: String!
  score: Int!
}
type Query { article: [article!]! @list }
```

`@primary_key(columns: [String!]!, constraint: String)` declares scalar GraphQL field
names, including composite keys. `constraint` defaults to the SQL table name followed
by `_pkey`. The declaration must match the database primary key. The generated query
field `<Type>_by_pk` takes each key as a required named argument and returns a row or null.

The generated `Mutation` root exposes the following fields for each table type `t`:

| Field | Arguments | Result |
|---|---|---|
| `insert_<t>` | `objects`, optional `on_conflict` | `affected_rows`, `returning` |
| `insert_<t>_one` | `object`, optional `on_conflict` | row or null |
| `update_<t>` | required `where`, optional `_set`, `_inc` | `affected_rows`, `returning` |
| `update_<t>_by_pk` | required `pk_columns`, optional `_set`, `_inc` | row or null |
| `delete_<t>` | required `where` | `affected_rows`, `returning` |
| `delete_<t>_by_pk` | each primary-key column | row or null |

The `_by_pk` fields require a primary-key declaration. `on_conflict` takes the declared
primary-key `constraint`, required `update_columns` and optional `where`. An empty
`update_columns` list means `DO NOTHING`. Conflict updates apply the update permission,
its presets, its row condition and its post-update check; inserted rows use the insert
check. Omitted insert fields use database defaults. Explicit null stays null. `_inc`
accepts numeric columns, and a column cannot occur in both `_set` and `_inc`.

Write directives are repeatable once per role and operation. Insert requires `columns`
and `check`; update requires `columns`, `where` and `check`; delete requires `where`.
Insert and update accept a `set` object of column presets. Presets use the same scalar
literal and session-reference syntax as read permissions. Preset columns are excluded
from user inputs even when listed in `columns`. The reserved `admin` role is unrestricted.
An operation without a role permission is absent from that role's schema. Row-returning
single-row operations require select permission; bulk operations can return only
`affected_rows` for a role with write permission alone.

`returning` supports the same row selections, relationships and relationship aggregates
as queries. Its top-level rows have select column restrictions but no select row filter.
Related scans receive their select row conditions. When a relationship returns to the
modified table, it reads the post-operation rows through a CTE combining changed rows
with unaffected snapshot rows. This preserves Hasura's returning semantics within one
statement; an ordinary read of the physical table inside a data-modifying CTE would
observe the pre-operation snapshot.

```graphql
mutation {
  first: insert_article_one(object: {id: 10, title: "Draft", score: 0}) { id }
  second: update_article_by_pk(pk_columns: {id: 10}, _inc: {score: 1}) { id score }
}
```

`Schema::compile` and `rusuna query compile-sql` accept query and mutation documents.
They return one `CompiledField` per root field in document order. **The caller must execute
all fields of a mutation, in order, on one transaction and roll it back if any field
fails.** The `rusuna` crate compiles statements; `rusuna_sqlx::execute` and the
async-graphql parts open the connection and the transaction.
The lower-level API is `Schema::compile_mutation(&MutationArguments, &[Selection],
&Session)`. `Table::primary_key` and `Table::write_permissions` provide the equivalent
schema declarations for `Schema::new`.

An insert or update permission check is a `WriteCheck.abort` node in the plan. SQL
lowers it to `CAST(CASE WHEN <check> THEN '1' ELSE 'rusuna: permission check failed'
|| left(s.ctid::text, 0) END AS integer)` in `RETURNING`. The empty suffix depends on
the affected row, preventing constant folding from rejecting a zero-row operation
whose permission check is constant false. A violating row fails the whole statement, including
when only `affected_rows` is selected and under a generic prepared plan. Callers can
recognize this failure with `is_permission_check_error(sqlstate, message)`, which checks
`PERMISSION_CHECK_SQLSTATE` (`22P02`) and `PERMISSION_CHECK_MARKER`. A failed statement
leaves PostgreSQL's transaction aborted until rollback. Hasura reports a GraphQL
`permission-error`; rusuna exposes a PostgreSQL error so checking a separate result
column cannot be forgotten.

Outside the mutation surface: nested relationship inserts, `update_<t>_many`, jsonb
operators such as `_append`, numeric operators other than `_inc`, and conflict constraints
other than the declared primary key. Conflict updates with primary-key presets are
rejected: the returning snapshot requires stable conflict keys. Ordinary updates can
change primary keys. SQL column types, database constraints, triggers
and defaults belong to the caller's database schema. The returning overlay covers
the explicit target table; it does not model trigger or cascade changes to other tables.
The overlay and the self-join that records original tuple identifiers are emitted only when
`returning` reads the modified table again through a relationship. The self-join matches
the declared primary-key columns, so under `READ COMMITTED` an update or delete applies to
a row that a concurrent transaction changed while the statement waited for its lock, and
`returning` observes the changed row once. An update or delete whose `returning` does not
read the modified table again follows PostgreSQL's ordinary re-check. Two cases remain:
when the concurrent transaction changes the row's primary key, the statement skips that
row; on a table without a primary-key declaration, the update self-join matches `ctid`
and skips a concurrently changed row, and a delete's returning relationships can still
include the deleted row's pre-change version.

## Running on PostgreSQL with sqlx

```rust
let fields = schema.compile(query, &variables, None, &session)?;
let data: serde_json::Map<String, serde_json::Value> = rusuna_sqlx::execute(&pool, &fields).await?;
```

`rusuna_sqlx::execute(&PgPool, &[CompiledField])` runs a query's root fields one after
another on one pooled connection. When any field is a mutation root, it runs all fields in
document order on one transaction, rolls back on the first error and commits otherwise.
`rusuna_sqlx::fetch(&mut PgConnection, &Compiled)` runs one statement, including on a
transaction the caller holds, and returns its JSON value (`null` for an absent row).

Parameters take the types PostgreSQL infers for them: `fetch` prepares the statement
(sqlx caches it per connection), reads the parameter types and encodes each `Value` for
its type. `Int` binds to `int2`, `int4` (both range checked), `int8`, `float4` and
`float8`; `Float` to `float4` and `float8`; `Bool` to `bool`; `Text` to `text`,
`varchar`, `bpchar` and `name`, and to `timestamptz` (RFC 3339, or
`YYYY-MM-DD HH:MM[:SS[.f]]` with a `±HH[:MM]` offset), `timestamp` (the same without an
offset) and `date` (`YYYY-MM-DD`). `Null` binds to any type. Other combinations are
`Error::Parameter` before the statement runs.

`rusuna_sqlx::Error` is `PermissionCheck` for a violated insert or update check
(`rusuna::is_permission_check_error`), `Parameter { index, message }` and
`Database(sqlx::Error)`. `PermissionCheck` displays Hasura's message,
`PERMISSION_CHECK_MESSAGE`.

## async-graphql schema

`rusuna-async-graphql` turns `Schema::expanded_sdl(role)` into the types of an
[async-graphql 7 dynamic schema](https://docs.rs/async-graphql/7.2.1/async_graphql/dynamic/index.html):
objects, input objects, enums, custom scalars and the root fields. Each rusuna root field
is resolved by one statement. Its resolver reads the field's alias, arguments with
variables substituted, and its selection with fragments expanded
(`Context::field()`). It writes them as a single-field GraphQL document whose arguments are
variables declared with the arguments' types, and compiles that document with
`Schema::compile`, the argument values as JSON variables and the request's `Session`, so
enum values arrive as JSON strings the way a client sends them. Fields below the root read
their values from the statement's JSON by response key, so no field below a root issues
SQL.

```rust
use async_graphql::dynamic::{Field, FieldFuture, FieldValue, Object, TypeRef};

let parts = rusuna_async_graphql::Builder::new(schema.clone(), "user")
    .field("article", "wordCount", TypeRef::named_nn(TypeRef::INT), &["title"], |ctx| {
        FieldFuture::new(async move {
            let title = rusuna_async_graphql::parent_column(&ctx, "title")?;
            Ok(Some(async_graphql::Value::from(
                title.as_str().unwrap_or_default().split_whitespace().count() as i32,
            )))
        })
    })
    .build()?;
let viewer = Object::new("Viewer").field(parts.root_field("article").unwrap());
let graphql = parts
    .query_field(Field::new("viewer", TypeRef::named_nn("Viewer"), |_| {
        FieldFuture::new(async { Ok(Some(FieldValue::owned_any(()))) })
    }))
    .schema()
    .register(viewer)
    .data(pool)
    .finish()?;
let response = graphql
    .execute(async_graphql::Request::new("{ viewer { article { title wordCount } } }").data(session))
    .await;
```

- **One schema per role.** `Builder::new(schema, role)` builds the types of that role's
  expanded SDL, so types, root fields, columns and operations the role may not use are
  absent from both introspection and execution. Build one async-graphql schema for each
  role (`rusuna::Schema::roles()`) and choose it per request; `rusuna-axum` does this.
  The resolver rejects a `Session` of another role with `access-denied`.
- **Session and pool.** The resolvers read `rusuna::Session` from the request data
  (`Request::data`) or the WebSocket connection data, and `sqlx::PgPool` (and
  `rusuna_sqlx::Subscriptions` for subscriptions) from the schema or request data. Authentication
  and the construction of the session belong to the caller.
- **Hand-written fields above rusuna.** `Parts::root_field(name)` returns a new
  `dynamic::Field` that resolves the query root `name` wherever it is registered, for
  example under a hand-written `viewer`. `Parts::query_field` and `Parts::mutation_field`
  add hand-written root fields, and `Parts::schema()` returns a `SchemaBuilder` with
  every rusuna type registered; register further hand-written types on it.
  `Parts::register(builder)` registers the same types on a builder the caller creates; pass
  `parts.subscription_name()` as its subscription root.
- **Hand-written fields on rusuna types.** `Builder::field(type, name, type_ref,
  requires, resolver)` adds a field to a rusuna row type. `requires` names scalar
  fields of that type. The statement selects each of them under the alias
  `__rusuna_<field>` whenever the hand-written field is selected, and
  `parent_column(&ctx, field)` reads it from the parent row. A role that cannot read the
  type or a required field does not get the hand-written field. Required names are at
  most 54 bytes so the alias fits the 63-byte limit. A type that is not a row type of
  the schema, or a required name that is not one of its scalar fields, is an error of
  `build` for every role.
- **Static types.** async-graphql 7 schemas are either static (`Schema<Query, Mutation,
  Subscription>` from derive macros) or dynamic; a dynamic schema cannot register a
  `#[derive(SimpleObject)]` or `#[Object]` type, and a static schema cannot hold dynamic
  types. Write the hand-written resolvers that share the schema with rusuna as
  `dynamic::Object` and `dynamic::Field`. Their resolvers can call the same async
  functions a static resolver calls, and can convert a serializable result with
  `async_graphql::to_value` and `FieldValue::value`.
- **Mutations.** A mutation operation runs its rusuna root fields in document order on
  one transaction, which the schema's extension opens with the first such field. After a
  field fails, the transaction is rolled back, later rusuna root fields do not run, and
  the response has `data: null` with the error. The transaction commits only when the
  response has no errors, so an error of a hand-written field also rolls back the rusuna
  writes. Hand-written mutation fields use their own connections. Two mutation root
  fields with the same response key are a `validation-failed` error: async-graphql
  7.2.1 resolves each occurrence separately, and a write runs once.
- **Subscriptions.** Each field of the role's `Subscription` root resolves to a stream: the
  resolver compiles the field like a query root field and subscribes to the statement on
  the `rusuna_sqlx::Subscriptions` it reads from the schema or request data (next section).
  `Parts::subscription_field` adds a hand-written `dynamic::SubscriptionField`. A role
  without query root fields has no `Subscription` root, and async-graphql 7.2.1 then
  answers every operation sent over WebSocket, including queries, with `Subscription root
  not found`; serve its queries over `POST`.
- **Errors.** Errors carry `extensions.code`. A violated insert or update check has
  Hasura's message `check constraint of an insert/update permission has failed` and code
  `permission-error`. Compilation errors are `validation-failed`; database errors are
  `constraint-violation` (SQLSTATE class 23), `data-exception` (class 22, and
  parameters that do not fit their column type) or `unexpected`; a missing or foreign
  session is `access-denied`. async-graphql reports validation errors and positions in
  its own words, and errors carry `locations` rather than Hasura's `extensions.path`.
- **Variables.** The extension applies variable defaults and binds omitted nullable
  variables to null before async-graphql validates and prunes `@skip` and `@include`;
  async-graphql 7.2.1 reads only the provided variables for those directives.
- **Introspection.** Introspection lists the types reachable from the root fields, equal
  to the role's expanded SDL except for comparison inputs no field uses. Object and input
  types left without fields are removed with the fields and nullable arguments that use
  them; a role without any root field needs a hand-written query field. `__typename` is
  answered by async-graphql, from the same type names. The tests of
  `crates/rusuna-http-tests` compare introspection with the expanded SDL of every role.

## axum routes

```rust
let app = axum::Router::new().route(
    "/api/graphql",
    rusuna_axum::graphql(schemas.clone(), |parts: &axum::http::request::Parts| {
        let session = authenticate(&parts.headers);
        async move { session }
    })
    .merge(rusuna_axum::graphql_ws(
        schemas,
        |parts: &axum::http::request::Parts, payload: serde_json::Value| {
            let session = authenticate_connection(&parts.headers, &payload);
            async move { session }
        },
    )),
);
```

`rusuna_axum::graphql(HashMap<String, dynamic::Schema>, session)` is a `POST` route. The
session function receives the request head and returns `Result<rusuna::Session,
axum::response::Response>`; an error response is sent as is. The route executes the
request on the schema of the session's role with the session in the request data, and
answers `access-denied` for a role without a schema. Request parsing and the response
are [async-graphql-axum](https://docs.rs/async-graphql-axum/7.2.1/async_graphql_axum/).

`rusuna_axum::graphql_ws(HashMap<String, dynamic::Schema>, session)` is a `GET` route that
upgrades to a WebSocket and speaks the
[`graphql-transport-ws`](https://github.com/enisdenjo/graphql-ws/blob/master/PROTOCOL.md)
protocol through async-graphql-axum. The client's first message must be `connection_init`. The session
function receives the upgrade request's head and that message's payload and returns
`Result<rusuna::Session, String>`; the session is fixed for the connection, so
authentication can read either the upgrade headers or the payload. The connection runs
every operation on the schema of the session's role with the session in the connection
data. Another first message closes the connection with code 4400; an error or a role
without a schema closes it with code 4403 and the message.

## Remote schemas

A remote schema puts the query and mutation root fields of another GraphQL server on the
role schemas, like Hasura's remote schemas. Clients select them in the same operation as
rusuna's root fields; rusuna forwards each remote root field to its server.

```rust
use rusuna_async_graphql::{Builder, RemoteSchema};

let weather = RemoteSchema::new("weather", "https://weather.example.com/graphql")?
    .header("authorization", &token)?
    .forward_header("x-request-id")?
    .roles(["admin", "user"])
    .introspect()
    .await?;
let parts = Builder::new(schema.clone(), "user").remote(weather.clone()).build()?;
```

```graphql
query { article(limit: 2) { id title } weather(city: "Oslo") { temperature station { name } } }
```

- **Types.** `introspect()` asks the server for its schema with the standard introspection
  query when the application starts, and is the usual way. `sdl(text)` takes the SDL
  instead, so a build needs no network and keeps a fixed copy of the remote schema. The
  remote types reachable from its query and mutation roots join the role schema:
  objects, input objects with their default values, enums, custom scalars, arguments with
  their defaults, and descriptions. Directive definitions are ignored. Interfaces,
  unions and type extensions are an error of `build`. Subscription root fields are not
  added.
- **Names.** A remote type whose name is already a type of the role schema (or of another
  remote schema), and a remote root field whose name is already a root field, are an
  error of `build`. `type_prefix(p)` renames every remote type except the built-in
  scalars, and `root_field_prefix(p)` every remote root field; the forwarded document uses
  the remote names. Hand-written types registered after `build` are not checked.
- **Roles.** `roles([...])` lists the roles whose schemas include the remote root fields
  and types; the default is `admin` only. `Builder::remote` on another role's builder adds
  nothing, so the fields are absent from that role's introspection and execution.
- **Forwarding.** A remote root field is sent as a document of that field alone, with
  the field's alias, arguments as variables declared with the remote's argument types,
  and its selection with fragments expanded. Arguments whose value is null, including
  omitted nullable variables, are left out, so the remote applies its own defaults; an
  explicit null argument is not sent. Each remote root field of a request is one
  HTTP `POST` (`content-type: application/json`, the fixed headers of `header`, and the
  client request headers named by `forward_header`). The client headers come from the
  request data `ClientHeaders`, which the `rusuna-axum` routes add. Remote values are read
  from the response like rusuna's own rows, so a remote field below the root issues no
  request. The client is `rusuna-http-client` (hyper 1 with rustls); `timeout` (default
  `REMOTE_TIMEOUT`, 60 seconds) bounds a request.
- **Errors.** When the server answers with `errors`, the root field is null and carries
  the message of the first remote error with `extensions.code` `remote-schema-error` and
  the remote errors in `extensions.remote_errors`. An unreachable server, a timeout or an
  answer that is not a GraphQL response give the same code with a message naming the
  remote schema. A `rest` endpoint answers such an error with status 502.
- **Mutations.** Remote mutation root fields run in document order with rusuna's, and the
  rule of repeated response keys applies to them. They are not part of rusuna's
  transaction: a failing remote field rolls back the rusuna writes of the operation and
  stops later root fields, but a remote write that succeeded stays, and a later failure
  cannot undo it.

Outside this feature: rusuna sends no session values to the remote server (forward
client headers when it needs them); there are no remote schema permissions that restrict remote fields or arguments per role
beyond the role list; there are no relationships between rusuna types and remote types
(Hasura's remote relationships); remote root fields are not batched into one request per
server; and remote subscriptions are not forwarded.

## REST endpoints

`rusuna_axum::rest` serves named GraphQL documents as REST routes, like Hasura's RESTified
endpoints. Each endpoint binds one query or mutation to a URL pattern and HTTP methods,
and runs it on the same role schemas, sessions, permissions and transactions as the
GraphQL route.

```rust
use rusuna_axum::{Endpoint, RestMethod};

let rest = rusuna_axum::rest(
    schemas.clone(),
    vec![
        Endpoint::new(
            "article",
            "/api/rest/articles/:id",
            [RestMethod::Get],
            "query Article($id: Int!) { article_by_pk(id: $id) { id title author { name } } }",
        ),
        Endpoint::new(
            "create_article",
            "/api/rest/articles",
            [RestMethod::Post],
            "mutation Create($title: String!) { insert_article_one(object: {title: $title}) { id } }",
        ),
    ],
    |parts: &axum::http::request::Parts| {
        let session = authenticate(&parts.headers);
        async move { session }
    },
)
.await?;
let app = axum::Router::new().route("/api/graphql", graphql_route).merge(rest);
```

`rest(HashMap<String, dynamic::Schema>, Vec<Endpoint>, session)` returns an
`axum::Router` or an `EndpointError { endpoint, message }`. The session function is the
one of `graphql`. `Endpoint::new(name, url, methods, query)` takes a distinct name, a
path whose segments `:name` bind the GraphQL variable `name`, the methods `Get`, `Post`,
`Put`, `Patch` and `Delete`, and a document with exactly one query or mutation operation.
`Endpoint` also deserializes from JSON, so endpoints can live in a file beside the SDL:

```json
[{"Name": "article", "Url": "/api/rest/articles/:id", "Methods": ["GET"],
  "Query": "query Article($id: Int!) { article_by_pk(id: $id) { id title } }"}]
```

**Building.** `rest` validates every document against the schema of every role, without
running a resolver: it executes the document in async-graphql's introspection-only mode,
which validates it and answers each field with null. A role whose schema rejects the
document, for example because a root field or column is hidden from it, cannot call the
endpoint. It is an error when no role can, when the document is a subscription or has
other than one operation, when a URL variable is not a variable of the operation or has a
list type, when two endpoints serve the same method at the same pattern, and when two
patterns differ only in variable names. Variables that the request must supply are not
required at this point.

**Variables.** A request builds the variables from three places; a later one replaces an
earlier one of the same name:

1. the JSON object of the body, for every method except `GET`, passed as is;
2. the query string;
3. the path variables.

Values from the path and the query string are text, converted by the variable's declared
type: `Int` and `Float` read a JSON number, `Boolean` reads `true` or `false`, and a bare
flag (`?published`) is `true`; an empty value of a nullable variable is null; every other
type (`String`, `ID`, `DateTime`, enums) is the text as a string. A list-typed variable
cannot come from the path or the query string. A name that is not a variable of the
operation is an error. Omitted variables take their defaults, and omitted nullable ones
are null, as in the GraphQL route.

**Responses.** A success is status 200 with the `data` object of the GraphQL response as
the body, for example `{"article_by_pk": {"id": 1, "title": "…"}}`. A failure is a JSON
body with `errors` (and `data`), each error with `extensions.code` where one applies:

| Status | When |
|---|---|
| 400 `bad-request` | The body is not a JSON object, a path or query value does not fit its type, or a name is not a variable. |
| 400 | async-graphql rejects a variable value, `validation-failed`, `data-exception`, `constraint-violation` and `permission-error` (a violated insert or update check). |
| 403 `access-denied` | The session's role has no schema or cannot call the endpoint. |
| 404, 405 | axum: no pattern matches, or the pattern has no endpoint for the method. |
| 500 `unexpected` | Another database error. |
| 502 `remote-schema-error` | A remote schema failed or answered with errors. |

Hasura's RESTified endpoints are declared in metadata and use a query collection; rusuna
takes them in Rust or JSON. rusuna rejects an endpoint at build time instead of at the
first request, and its error bodies are GraphQL error lists. Caching (`@cached`) and the
OpenAPI export are outside this crate.

## Subscriptions

```rust
let subscriptions = rusuna_sqlx::Subscriptions::new(
    pool.clone(),
    rusuna_sqlx::SubscriptionOptions { interval: Duration::from_secs(1), batch_size: 100 },
);
let graphql = rusuna_async_graphql::Builder::new(schema.clone(), "user")
    .build()?
    .schema()
    .data(pool.clone())
    .data(subscriptions.clone())
    .finish()?;
```

A subscription root field has the name, arguments and selection of the query root field
of the same name (`<t>`, `<t>_aggregate`, `<t>_by_pk`). Subscribing sends the field's
current result, then a new result whenever it changes, like a Hasura v2 live query. The
statement is the one the query compiles to, so the role's read permissions (row
conditions and columns) apply to the subscription as they apply to the query, and the
permission theorems hold for it unchanged.

`rusuna_sqlx::Subscriptions` polls the statements. Subscriptions with the same role and
the same SQL text form a group: `compile_erase` proves that inputs differing only in
values (variables and session values) compile to the same text, so these are the
subscriptions that differ in values only. Every `interval` (default one second) each
group runs one statement for up to `batch_size` (default 100) subscriptions, and sends a
subscription its result when it differs from the last result sent to it (JSON
equality). Subscribing runs the group's statement for the new subscription alone: its
result is the first result, sent at once, and an invalid value or a failing statement
fails that subscription without joining the group. The interval counts from the end of a
run. When the statement of a batch fails, each subscription of the batch runs alone, so a
database error reaches only the subscriptions whose own statement fails; the group keeps
polling and sends a later success again. A group exists while it has subscriptions:
`complete`, a closed connection or dropping a `rusuna_sqlx::Subscription` leaves the group,
and the last one leaving stops its runs. `Subscriptions::stats()` reports the groups,
subscriptions, runs and the statements of runs.

The statement of a group is `rusuna_sqlx::Multiplexed`. It reads one `jsonb[]` parameter,
the parameters of each subscription as one JSON array, and runs the compiled statement
once per array in a `LEFT JOIN LATERAL` subquery. For the role `user` of
`tests/fixtures/permissions.graphql` and
`subscription($min: Int!) { article(where: {id: {_gte: $min}}, order_by: {id: asc}) { id title } }`:

```sql
SELECT "__rusuna_subscription"."id", "__rusuna_result"."value" FROM unnest($1::jsonb[]) WITH ORDINALITY AS "__rusuna_subscription"("args", "id") LEFT JOIN LATERAL (WITH "r0" AS ( SELECT "s".* FROM "public"."article" AS "s" WHERE true AND ( ( true AND ( "s"."id" >= (("__rusuna_subscription"."args"->>0)::int4) ) AND ( "s"."author_id" = (("__rusuna_subscription"."args"->>1)::int4) ) ) ) ORDER BY "s"."id" ASC NULLS LAST ) SELECT coalesce(json_agg( row_to_json((SELECT "%" FROM (SELECT "t0"."id" AS "id" , "t0"."title" AS "title" ) AS "%")) ORDER BY "t0"."id" ASC NULLS LAST ), '[]') FROM "r0" AS "t0") AS "__rusuna_result"("value") ON true ORDER BY "__rusuna_subscription"."id"
```

with one array per subscription, `["2","3"]` for `$min = 2` and `user_id = 3`. Each
placeholder `$k` of the compiled statement becomes element `k - 1` of the array, cast to
the type PostgreSQL infers for `$k` when `Multiplexed::prepare` prepares the compiled
statement. The values are written as text in the form `rusuna_sqlx::fetch` would send:
integers and floats in decimal (`float4` values rounded to `f32` first), date and time
values as the microsecond `fetch` binds, in ISO 8601 with years past 9999 written in
full and `BC` for years before 1. The types are those `fetch` binds; another inferred type makes `prepare`
fail. The rewrite copies single-quoted literals and double-quoted identifiers
unchanged and requires the placeholders outside them to be exactly `$1` to `$n` in order,
which `placeholders_numbered` proves for every compiled statement.

On the benchmark data (`bench/seed.sql`), the statement of
`article(where: {title: {_like: $t}}, order_by: {id: asc}) { id title comments { body } author { name } }`
for 100 subscriptions with different `user_id` values executed in 98 to 107 ms
(`EXPLAIN ANALYZE`, 7 runs each under `plan_cache_mode = auto` and `force_generic_plan`).
The 100 single statements under a generic plan took 112 ms in total by `psql`'s `\timing`,
round trips on a Unix socket included. A per-row argument in the lateral subquery uses the index
on `article.author_id` as a parameter would.

Differences from Hasura v2 live queries:

- Hasura groups the subscriptions with equal variables and session values into cohorts and
  runs the statement once per cohort; rusuna runs it once per subscription, and compares
  results per subscription.
- Hasura compares hashes of the results; rusuna compares the JSON values it keeps for each
  subscription.
- The `subscription_root` of Hasura also has `<t>_stream` fields (streaming subscriptions);
  rusuna has none.
- Hasura sends the first result from the next poll; rusuna runs the statement for the new
  subscription when it subscribes.

Lists without `order_by` follow PostgreSQL's row order, which can differ between runs
and then sends a result with the same rows in another order.

## Event triggers and cron triggers

`rusuna-events` calls your code when rows change and at the times of cron expressions.
A PostgreSQL row trigger records each change in a table; a worker inside your process
delivers the recorded events to async Rust handlers or webhooks, and retries failed
deliveries.

```rust
use rusuna_events::{Cause, CronTrigger, Event, EventTrigger, Events, Operation, Retry, Webhook, WorkerOptions};
use std::time::Duration;

let events = Events::new()
    .trigger(EventTrigger::new("article_created", "article").operations([Operation::Insert]))
    .trigger(
        EventTrigger::new("article_renamed", "article")
            .operations([Operation::Update])
            .columns(["title"]),
    )
    .cron(CronTrigger::new("nightly_report", "0 3 * * *", serde_json::json!({"kind": "daily"}))?)
    .handler("article_created", Retry::default(), |event: Event| async move {
        if let Cause::Row { new: Some(row), .. } = &event.cause {
            send_welcome_mail(&row["author_id"]).await?;
        }
        Ok(())
    })
    .webhook(
        "article_renamed",
        Retry { max_retries: 5, interval: Duration::from_secs(30), timeout: Duration::from_secs(10) },
        Webhook::new("https://search.example.com/reindex")?.header("authorization", &token)?,
    )
    .handler("nightly_report", Retry::default(), |_event: Event| async move { report().await });

events.migrate(&pool).await?;
let worker = events.worker(pool.clone(), WorkerOptions::default())?;
let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
let worker = tokio::spawn(worker.run(async { let _ = stopped.await; }));
// ... serve requests ...
let _ = stop.send(());
worker.await?;
```

### Declaring triggers and the migration

An `EventTrigger` names a trigger and declares its SQL table (`schema` defaults to
`public`), the operations that record an event (all three by default) and, for updates,
the SQL columns an update must change (`columns`; empty means every update). Trigger
names are 1 to 42 ASCII letters, digits, `_` or `-`. Declarations live in Rust, next to
the handlers. The same declaration has a JSON form with the keys `Name`, `Table`,
`Schema`, `Operations` (`"INSERT"`, `"UPDATE"`, `"DELETE"`) and `Columns`, which
`EventTrigger` deserializes with serde and the CLI reads:

```sh
rusuna events generate-migration --triggers file://events.json --output text > migrations/0005_events.sql
```

`rusuna_events::migration(&triggers)` (and `Events::migration`) returns the same SQL.
Add it to your own migrations, or apply it with `rusuna_events::migrate(&pool,
&triggers)` (`Events::migrate`), which runs it in one transaction under an advisory lock.
The script creates:

- the schema `rusuna` and the tables `rusuna.event_log` and `rusuna.event_invocation`
  (below), with `CREATE … IF NOT EXISTS`;
- the trigger function `rusuna.capture_event()` (`CREATE OR REPLACE FUNCTION`, PL/pgSQL,
  `SECURITY DEFINER` with a fixed `search_path`, owned by the role that applies the
  migration);
- one `AFTER … FOR EACH ROW` trigger `rusuna_event_<name>` per declaration
  (`CREATE OR REPLACE TRIGGER`), for example

```sql
CREATE OR REPLACE TRIGGER "rusuna_event_article_renamed" AFTER UPDATE OF "title" ON "public"."article" FOR EACH ROW EXECUTE FUNCTION rusuna.capture_event('article_renamed', 'title');
```

It uses only standard PostgreSQL: no extension and no superuser, so it runs on Amazon RDS
and Aurora. It requires PostgreSQL 14 or later (`CREATE OR REPLACE TRIGGER`;
`gen_random_uuid()` is built in from 13). The script has no transaction statements, so a
migration tool can wrap it in its own.

Privileges: the role that applies the migration needs `CREATE` on the database (for the
schema `rusuna`) and must own the tables that get triggers. The roles that write your
tables need no privilege on `rusuna`, since the function runs as its owner. The worker's
role needs

```sql
GRANT USAGE ON SCHEMA rusuna TO worker_role;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA rusuna TO worker_role;
```

(`INSERT` and `DELETE` for cron occurrences, `schedule` and `cleanup`).

The script is idempotent: applying it again changes nothing, and applying it with changed
declarations replaces the trigger of each declared name on its table. It does not drop
anything. When you remove a declaration or move it to another table, drop the old trigger
in your migration: `DROP TRIGGER IF EXISTS "rusuna_event_<name>" ON <table>;`. The table
layout belongs to this version of the crate.

The function writes `to_jsonb(OLD)` and `to_jsonb(NEW)`, keyed by SQL column names, and
sends `NOTIFY rusuna_events`. It runs in the transaction of the change, so every
`INSERT`, `UPDATE`, `DELETE` and `COPY` records its event, whether it came through rusuna,
another client or `psql`, and a rolled-back change records nothing. As with every
ordinary row trigger, `TRUNCATE` records nothing, and a session with
`session_replication_role = replica` (logical replication apply, some bulk loads)
records nothing. An update trigger with columns fires when the update
lists one of the columns (`UPDATE OF`) and one of their values changes (`IS DISTINCT FROM`
on the JSON values); a column changed only by another `BEFORE` trigger does not count.

### The event table

`rusuna.event_log` has one row per event:

| Column | Meaning |
|---|---|
| `id uuid` | `gen_random_uuid()`. |
| `kind text` | `row`, `cron` or `scheduled`. |
| `trigger_name text` | The trigger name; deliveries are looked up by it. |
| `schema_name`, `table_name text` | The changed table (row events). |
| `operation text` | `INSERT`, `UPDATE` or `DELETE` (row events). |
| `old`, `new jsonb` | The row before and after the change; null for an insert's `old` and a delete's `new`. |
| `payload jsonb` | The payload of a cron or scheduled event. |
| `scheduled_at timestamptz` | The occurrence of a cron event, the due time of a scheduled event. |
| `created_at timestamptz` | When the event was recorded (`now()` of the recording transaction). |
| `status text` | `pending`, `delivered` or `failed`. |
| `tries integer` | Attempts started. |
| `next_attempt_at timestamptz` | When the next attempt is due; null means at once. |
| `locked_until timestamptz` | The lease of the current claim. |
| `completed_at timestamptz` | When the event became `delivered` or `failed`. |

`rusuna.event_invocation` keeps one row per recorded attempt: `event_id`, `attempt`,
`succeeded`, the HTTP `status` of a webhook, the `error` text (a handler's error, the first
1000 characters of a webhook's response, a timeout or a panic), `started_at` and
`finished_at`. Rows are deleted with their event.

### Delivery

`Events::handler(name, retry, f)` delivers the events of `name` to an async function
`Fn(Event) -> impl Future<Output = Result<(), HandlerError>>`. `Events::webhook(name,
retry, webhook)` sends them as an HTTP `POST` with `content-type: application/json` and the
headers of the `Webhook`; a 2xx status is a success. A name has one delivery. The client is
hyper 1 with rustls (the ring provider and the webpki root certificates), so it builds
without OpenSSL or CMake.

`Event` has the event's `id`, `trigger`, `cause` (`Cause::Row { schema, table, op, old,
new }`, `Cause::Cron { scheduled_at }` or `Cause::Scheduled { scheduled_at }`), `payload`,
`created_at`, `attempt` (from 1) and `max_retries`. `Event::body()` is the JSON a webhook
receives. For a row event it has the shape of a Hasura v2 event trigger payload:

```json
{
  "id": "6c1f…",
  "created_at": "2026-01-01T00:00:00.000000Z",
  "trigger": {"name": "article_created"},
  "table": {"schema": "public", "name": "article"},
  "event": {"op": "INSERT", "data": {"old": null, "new": {"id": 100, "title": "Ships and software"}}},
  "delivery_info": {"max_retries": 0, "current_retry": 0}
}
```

A cron or scheduled event sends `{"id", "name", "scheduled_time", "payload"}`.

`Retry` sets per name the retries after the first attempt (`max_retries`, default 0), the
time between a failed attempt and the next one (`interval`, default 10 seconds) and the
time an attempt may take (`timeout`, default 60 seconds). An error, a handler panic, a
non-2xx status, a connection error or the timeout fails the attempt; at the timeout the
handler's future is dropped. An event whose attempts reach `max_retries + 1` without a
success stays in the table as `failed` and is not tried again.
`rusuna_events::redeliver(executor, id)` makes a failed event pending again with its
attempts counted from zero.

**Delivery is at least once.** A handler or webhook can receive an event again: when the
worker stops after the delivery succeeded but before it recorded the result, or when an
attempt outlives its lease. Write handlers to be idempotent, for example keyed by
`Event::id`. Events are delivered concurrently and in no guaranteed order.

### The worker

`Events::worker(pool, options)` checks the definitions and returns a `Worker`;
`worker.run(shutdown)` is the future to spawn. Each poll records due cron occurrences, then
claims up to `concurrency` pending events of the names this worker delivers, oldest first:

```sql
… SELECT … FROM rusuna.event_log … WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= $now)
  AND (locked_until IS NULL OR locked_until <= $now) ORDER BY created_at, id LIMIT $n FOR UPDATE OF e SKIP LOCKED
```

and in the same statement increments `tries` and sets `locked_until` to now plus the name's
`timeout` plus `WorkerOptions::lease_margin` (default 30 seconds). Deliveries run outside
that transaction, so no connection or lock is held while a handler runs. The result is
recorded only if `tries` still equals the claimed attempt and the event is pending, so a
claim that another worker has since taken over cannot overwrite the newer result.

- **Several workers.** Workers in one process or in many claim different rows:
  `SKIP LOCKED` skips the rows another claim is taking, and a claimed row has a lease.
- **A worker that stops in the middle.** Aborting the worker's task drops its deliveries
  in progress; a crashed process loses them too. Their events stay pending with a lease,
  and any worker claims them again when the lease expires. The interrupted attempt counts
  as an attempt; the event is claimed again even past `max_retries`, since no result was
  recorded.
- **Waiting.** A worker polls every `poll_interval` (default one second). With `listen`
  (default true) it also `LISTEN`s on `rusuna_events` and polls at once when the trigger
  function notifies. The listener (sqlx `PgListener`) keeps one connection of the pool for
  the worker's lifetime; size the pool for it. Polling alone is enough for correctness,
  including behind poolers that do not support `LISTEN`. When a poll fills
  every free slot, the worker polls again as soon as a delivery finishes.
- **Shutdown.** When the `shutdown` future completes, the worker stops claiming and
  waits for its deliveries in progress, at most their timeouts, to record their results.
- **Clock.** `Worker::clock(Arc<dyn Clock>)` replaces the system clock. The clock decides
  which events are due, the leases, the retry times and the cron occurrences, so tests
  move time without waiting. Events recorded by triggers have a null `next_attempt_at`,
  so they are due under any clock. A lease set by one worker is compared with the clock of
  the worker that claims next, so the clocks of hosts that share a database must agree
  within `lease_margin`.
- **Counters.** `Worker::monitor()` returns a handle whose `stats()` reports polls,
  claimed events, delivered and failed attempts, and database errors. A database error
  does not stop the worker; it tries again at the next poll.

`rusuna_events::cleanup(executor, before)` deletes delivered events completed before
`before`, with their invocations, and keeps the latest occurrence of each cron trigger.
Run it on a schedule of your own, for example from a cron trigger. Failed and pending
events stay.

### Cron triggers and scheduled events

`CronTrigger::new(name, expression, payload)` takes a five-field expression (minute,
hour, day of month, month, day of week), evaluated in UTC with
[croner](https://docs.rs/croner/4.0.3/croner/) 4.0.3. A seconds field and a year field
are rejected. A cron trigger needs a handler or webhook of the same name.

On each poll a worker inserts the occurrences that are due since its last poll as
`kind = 'cron'` events with `INSERT … ON CONFLICT (trigger_name, scheduled_at) WHERE kind
= 'cron' DO NOTHING`. A unique index on the trigger name and the occurrence time makes the
first insert win, so an occurrence becomes one event however many workers run, and
it is delivered like every other event.

Missed occurrences: a starting worker reads the latest recorded occurrence of each cron
trigger. If there is one, it records the occurrences after it that are not older than the
trigger's `tolerance` (default six hours, `CronTrigger::tolerance`), at most 1000 per poll;
older ones are skipped. A cron trigger with no recorded occurrence starts with the next
occurrence after the worker starts. `cleanup` keeps the latest occurrence for this reason.

`rusuna_events::schedule(executor, name, at, payload)` records a one-off event of `name`
due at `at` and returns its id. Pass a transaction to record it only if the transaction
commits. It is delivered to the handler or webhook of `name`.

### Differences from Hasura

- Triggers and deliveries are declared in Rust (or JSON for the CLI), not in metadata, and
  their SQL is a migration you apply.
- The payload has no `event.session_variables` and no `trace_context`: the trigger
  function does not read a session from the database. There is no `MANUAL` operation.
- Failures are retried at a fixed interval; a webhook's `Retry-After` header is not read.
- A worker runs in your process; there is no separate event delivery service.
- Cron triggers are declared in code, and their payload shape is rusuna's own.

Outside this crate: a console for events, request and response transforms, per-trigger
column selections for the payload, and dropping triggers of removed declarations.

## Example server

`crates/rusuna-axum/examples/blog.rs` serves `blog.graphql` with roles `reader` and
`writer`, chosen by the `x-role` header, with `x-user-id` as the `user_id` session value.
It adds the hand-written `viewer { role article }` and `article.wordCount`.

```sh
nix develop --command tools/with-postgres.sh crates/rusuna-axum/examples/blog.sql -- \
  cargo run -p rusuna-axum --example blog
```

In another terminal:

```sh
curl -s http://127.0.0.1:3000/api/graphql -H 'content-type: application/json' -H 'x-role: reader' \
  -d '{"query":"{ article(order_by: {id: asc}) { id title wordCount author { name } } }"}'
curl -s http://127.0.0.1:3000/api/graphql -H 'content-type: application/json' -H 'x-role: writer' -H 'x-user-id: 2' \
  -d '{"query":"{ viewer { role article(order_by: {id: asc}) { id title published } } }"}'
curl -s http://127.0.0.1:3000/api/graphql -H 'content-type: application/json' -H 'x-role: writer' -H 'x-user-id: 2' \
  -d '{"query":"mutation { insert_article_one(object: {title: \"Ships and software\", published: true}) { id author { name } } }"}'
```

A reader sees published articles only; a writer also sees its own drafts and inserts
articles as itself. `tools/with-postgres.sh` sets `DATABASE_URL`, which the example
reads.

The example also serves the REST endpoint `/api/rest/articles/:id`, the document
`query Article($id: Int!) { article_by_pk(id: $id) { id title author { name } } }`:

```sh
curl -s http://127.0.0.1:3000/api/rest/articles/1 -H 'x-role: reader'
curl -s http://127.0.0.1:3000/api/rest/articles/2 -H 'x-role: writer' -H 'x-user-id: 2'
curl -s -i http://127.0.0.1:3000/api/rest/articles/x -H 'x-role: reader'
```

The first answers `{"article_by_pk":{"id":1,"title":"Notes on the analytical engine","author":{"name":"Ada"}}}`.
The draft 2 is `null` for a reader and the row for its author. The last answers status
400 with the code `bad-request`, since `x` is not an `Int`.

The same path serves subscriptions over WebSocket. The example reads the role and the
user from the `headers` object of the `connection_init` payload. With
[websocat](https://github.com/vi/websocat) (`nix shell nixpkgs#websocat`), connect and
send the two messages:

```sh
websocat --protocol graphql-transport-ws ws://127.0.0.1:3000/api/graphql
{"type":"connection_init","payload":{"headers":{"x-role":"writer","x-user-id":"2"}}}
{"id":"1","type":"subscribe","payload":{"query":"subscription { article(order_by: {id: asc}) { id title published } }"}}
```

The server answers `connection_ack` and the current articles. A change of the result,
such as publishing the draft of author 2 from another terminal, sends the new list:

```sh
curl -s http://127.0.0.1:3000/api/graphql -H 'content-type: application/json' -H 'x-role: writer' -H 'x-user-id: 2' \
  -d '{"query":"mutation { update_article_by_pk(pk_columns: {id: 2}, _set: {published: true}) { id published } }"}'
```

Send `{"id":"1","type":"complete"}` to end the subscription.

The example also declares the event trigger `article_created` on inserts into `article`,
applies its migration at startup and runs a worker beside the server. Each inserted
article, such as the one of the `insert_article_one` request above, makes the server
print a line from the handler:

```
article 100 created: Ships and software
```

Ctrl-C stops the server, then the worker after its deliveries in progress.

## CLI

```sh
nix run . -- schema expand-sdl --role admin --schema file://tests/fixtures/schema.graphql --output text
nix run . -- query compile-sql --role admin --schema file://tests/fixtures/schema.graphql --query file://tests/fixtures/queries/a.graphql
nix run . -- query compile-sql --role user --session '{"user_id":3}' --schema file://tests/fixtures/permissions.graphql --query '{ article { id } }'
nix run . -- events generate-migration --triggers file://crates/rusuna-cli/tests/fixtures/events.json --output text
```

`rusuna-cli` installs the binary `rusuna`. Values beginning with `file://` are read from
files, including `file:///dev/stdin`. `query compile-sql` accepts `--operation-name` and
`--variables` and `--session` (JSON objects or files). `schema expand-sdl` and
`query compile-sql` require `--role`. `events generate-migration` takes `--triggers`, a
JSON array of event triggers (default `[]`, the tables and the function only). Global options are `--output json|text|table`
(default `json`), `--debug`, `--no-debug` and `--version`. Each command level has `help`.
JSON keys are `Sdl`, `Sql` (the migration), or `Fields` containing `ResponseKey`, `Sql` and `Params`. Text
expansion and the migration are raw text; query text is tab-separated and table output adds column names.
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
`order_by: Vec<Order>`, `limit: Option<u64>`, `offset: Option<u64>`,
`fields: Vec<Selection>` and `arguments: Vec<(String, Value)>` (the arguments of a native
query root, in declared order). Its builders are `alias`, `filter`, `order_by`, `limit`,
`offset`, `field` and `argument`. `filter` sets the entire expression; combine predicates explicitly:

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
- A role's permission `limit` becomes the `LIMIT` of a list root or an array relationship,
  which is then read like one with a query `limit`. A bounded `nodes` list takes the first
  rows of the ordered aggregate: `array_to_json((array_agg(… ORDER BY …))[1:n])`.
- A native query is read as `(<statement>) AS "s"`, with each `{{name}}` replaced by a
  parameter.
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
| `run_params`, `compile_params`, `resolve_values` | Parameters follow document order: the arguments of a native query root in the order of their markers, then the filter values, with each scan's bound permission values after its user filter. |
| `compile_erase` | Resolved plans that differ only in their query or session-derived values produce the same SQL text. |
| `placeholders_numbered`, `slots_numbered` | The placeholders are `$1` … `$n`, in the order of the parameters. |
| `unquote_quote`, `unquote_quoteChars` | `unquoteChars`, a reader of a double-quoted identifier written after PostgreSQL's lexical rules, reads a quoted name back as the name and stops at its closing quote. |
| `unquote_quoteString`, `unquote_quoteStringChars` | Reading a quoted string literal returns the original string and stops at the closing quote. |
| `resolve_declared` | Every scan in the plan, including relationship predicates inside `EXISTS`, reads a declared type: a table scan its declared SQL table, a native query scan a text equal to the declared statement once the values are erased. |
| `distinct_iff` | The check for repeated aliases accepts exactly the lists without duplicates. |
| `resolve_scan_conditions`, `resolve_tables` | Every non-admin scan, including scans inside permission predicates, contains the bound condition of that role and has a permission definition. |
| `resolve_columns` | User column references in selections, filters, ordering and aggregates belong to the role's visible columns; join keys and policy columns are excluded. |
| `resolve_limits`, `capLimit_cap`, `capLimit_user`, `capLimit_choice` | For a non-admin role whose permission has a `limit`, every list root, array relationship and aggregate `nodes` list of the plan has a limit of at most that value. The applied limit is at most the permission's and the query's and equals one of them. |

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
| `limitScan_exact`, `listScan_array`, `listScan_other`, `limitNodes_limit`, `listChildren_limits`, `listChildren_other` | A row limit is applied to the scan of a list and the `nodes` of an aggregate, and nothing else changes. |
| `sourceToks_table`, `sourceToks_tokens`, `sourceDeclared_iff` | A scan emits its table or the tokens of its statement, and the declaration check accepts exactly the declared table or the declared statement with its values erased. |
| `argumentValid_iff`, `acceptsArguments_iff`, `resolveSource_exact` | Root arguments are accepted when their names are the declared ones in order and no value is a nonfinite float; the statement is then filled with them. |
| `resolveWrite_permissions`, `resolveWrite_user_columns` | Aborting checks, operation row conditions and authorized writes; user columns exclude presets. |
| `resolveWrite_declared`, `resolveWrite_tables` | Physical write targets are declared and have the requested operation permission. |
| `resolveWrite_returning_conditions` | Relationship and predicate scans in returning carry their read permission conditions. |
| `resolveSelection_accepts`, `runSelection_accepts` | Pre-permission resolution and compilation succeed exactly when the Boolean `acceptsSelection` accepts the schema and selection. |

[leanassay](https://github.com/conao3/lean-leanassay/tree/fc0d05473917864cedb6d27e208c81de3e2a11af) checks which theorems rule out
broken implementations: the audit covers thirty-eight functions, with 236 mutations, 161 killed,
23 equivalent on the sampled inputs, 52 stillborn, zero holes, zero untested and zero timed out.
There are thirteen independent properties (twenty-four principal theorem statements), thirty-one
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
| Selection resolution | 2 | 136 | 40 | 3 | 14 | 0 | 25 | 54 |
| Permission resolution | 3 | 1 | 0 | 1 | 0 | 0 | 0 | 0 |
| Write emission | 2 | 87 | 0 | 73 | 10 | 0 | 0 | 4 |
| Write resolution | 3 | 56 | 1 | 19 | 32 | 0 | 0 | 4 |
| Total | 18 | 456 | 64 | 196 | 58 | 0 | 25 | 113 |

The survey uses 1000 samples and a 30-second deadline per mutation. Timeouts are
unresolved and depend on machine load. Equivalent means equal outputs on the sampled
inputs; untested means an output comparison could not be performed. Stillborn
mutations do not typecheck. The holes concern SQL text, acceptance rules and the
parameters of write statements, which are outside the properties proved above; the
tests cover these by example.

Outside the proofs:

- What PostgreSQL returns. The row permission theorem proves that each `Scan` contains
  its role condition as an expression in the plan. It does not interpret SQL execution,
  or prove that the expression represents the application author's intended policy.
  Differential tests compare PostgreSQL results with Hasura-generated statements.
- The complete acceptance rules of the role-aware `resolve` and `run`. The proved
  `acceptsSelection` equivalence covers the schema supplied to pre-permission
  resolution. Session binding and the final permission-dependency validator need
  an input-only success characterization. Error variants remain outside the
  acceptance theorem. Tests cover these cases by example.
- leanfran, which translates the Lean definitions to the Rust that runs. The tests run
  the generated Rust.
- `src/lib.rs`, which converts public Rust types to Lean types and back; SDL parsing,
  role-specific SDL rendering and scalar type checking in `src/graphql/`; authentication
  and construction of the session by the caller.
- `rusuna-sqlx`, `rusuna-async-graphql` and `rusuna-axum`: parameter encoding, the
  dynamic schema, the selection hand-off, transactions, HTTP and WebSocket. The HTTP tests
  cover them.
- The row limits inside a mutation's `returning`. `resolve_limits` concerns query plans; the
  mutation fixtures of a limited role test `returning`.
- The multiplexed subscription statement. `rusuna-sqlx` rewrites the placeholders of the
  SQL text in Rust; `placeholders_numbered` fixes what it expects, and a test runs the
  rewritten statement of every admin and permission fixture against its single statements.
- `unquoteChars` as a description of PostgreSQL's reader.
- The statements of native queries. They are trusted input of the schema author; the proofs
  show that their text reaches the SQL unchanged and their arguments only as parameters, not
  what PostgreSQL computes from them.
- `rusuna-events`: the trigger function, the claim and lease statements, cron occurrences
  and webhook delivery. The tests in `crates/rusuna-events/tests` cover them on PostgreSQL.

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
| A: 20 articles, their author and 5 comments each | 0.5 (0.4–2.2) | 0.5 (0.4–1.0) | 0.6 (0.5–1.0) | 0.5 (0.4–0.9) |
| B: 50,000 articles and their author | 137.9 (115.9–185.4) | 196.0 (177.7–259.6) | 136.8 (118.0–154.5) | 172.8 (157.0–246.6) |
| C: 2,000 authors, all their articles and all their comments | 601.5 (509.3–778.6) | 646.8 (579.8–713.5) | 679.3 (602.4–765.3) | 637.2 (551.6–740.0) |

All three speed goals pass in this run. Compiling these GraphQL documents, including parsing and type checking, takes 93 to 158 µs.
All four implementations return equal results on the benchmark data. The differential
test covers 43 admin queries and 19 permission queries, including empty results, nested
aggregates, custom root names, nullable columns, composite relationship predicates and
every supported comparison. Ten rejected role queries are fixtures too. The permission
fixtures use two roles and session IDs 1 and 3, covering roots, object and array
relationships, set and lateral aggregation, and nested `EXISTS`. Each captured Hasura
statement was also compared with its role-bearing HTTP response. The exported metadata
is in `tests/fixtures/permissions/metadata.json`.

With role `user` and `user_id = 3`, the same benchmark measures A at 1.0 ms (0.6–4.4),
B at 0.2 ms (0.2–0.6), and C at 4.2 ms (3.8–6.8), 15 runs each. These smaller result
sets also match Hasura.

The same comparisons run over HTTP in `crates/rusuna-http-tests`: an axum server built
with `rusuna-axum`, one async-graphql schema per role and hand-written `viewer` and
`article.wordCount` fields answers all 43 admin queries and 19 permission queries with
the `data` of the Hasura statements, and all 64 mutation fixtures with Hasura's
responses and table states, including 7 permission-check violations with Hasura's message
and code, and the multi-root mutations. Further tests cover a failing third root field
that rolls back the first two, variables with defaults, omitted variables, enum and
`on_conflict` inputs as variables, repeated mutation response keys, fragments and
`@skip`, hidden types and root fields per role, and introspection against the expanded
SDL of 12 role and schema pairs. `crates/rusuna-sqlx/tests` binds every supported
parameter type and checks the mutation transaction without async-graphql.

`crates/rusuna-http-tests/tests/views.rs` maps a view and a materialized view
(`tests/fixtures/views.sql` and `views.graphql`): relationships from and to them, nested
and root aggregates and `<t>_by_pk` on the materialized view return the rows of the views;
a role sees only the rows and columns its permissions grant on them; and introspection, the
expanded SDL, a mutation request and `compile_mutation` show that read-only types have no
mutation. A subscription to the view receives the change of a base table, and one to the
materialized view receives the change after `REFRESH MATERIALIZED VIEW`.

`crates/rusuna-http-tests/tests/rest.rs` serves five endpoints over the mutation fixtures.
A `GET` with a path variable, one with query-string values (a number, a bare Boolean flag
and a percent-encoded string), a `POST` with a JSON body and a `PATCH` and `PUT` that mix
the path with the query string or the body each answer the `data` of the same document
sent to the GraphQL route, for `admin` and for `user` with the presets and row conditions
of its permissions; a path variable replaces a body value of the same name. Values that do
not fit their types, unknown names, bodies that are not JSON objects, missing required
variables and a violated insert check answer 400 and change no row; roles whose schema
rejects the document answer 403. Building the routes runs no resolver, even for a
mutation, and rejects subscriptions, unknown fields, unbound URL variables, documents with
two operations, invalid patterns and conflicting endpoints.

`crates/rusuna-http-tests/tests/remote.rs` serves a weather GraphQL server (axum and an
async-graphql dynamic schema) on `127.0.0.1:0` inside the test, and adds it to the schemas
of `tests/fixtures/permissions.graphql`. One request selects rusuna and remote root fields
with variables, a fragment, an alias and an enum argument; the remote part equals the
server's own answer. A mutation updates a table and the remote server in one operation;
when the remote field fails, the table write is rolled back and later remote fields do not
run, and repeated remote response keys are rejected. Introspection lists the remote
types, descriptions, enum values and input defaults for `admin` and `user` and nothing for
`reviewer`, which cannot select the fields. Fixed and forwarded headers reach the server
and other client headers do not; a remote error, an unreachable server and introspecting
it are errors with their messages. Same-named types or root fields, interfaces, unions,
undefined types, invalid SDL and a missing SDL fail `build`; with prefixes, a renamed
input object and enum argument are forwarded under the remote names, one request per
remote root field. Enum values come back from nested and root remote fields, and an
omitted nullable variable leaves the remote argument default in effect. A REST endpoint
over an unreachable remote answers 502. Schemas built from introspection and from SDL
answer queries and introspection alike.

The subscription tests in `crates/rusuna-http-tests/tests/subscriptions.rs` run over
WebSocket. A subscription receives its first result, then the results changed by a
mutation through rusuna and by plain SQL, and nothing after a write that leaves its result
unchanged or that touches rows its role and session cannot read. 100 subscriptions with
four `user_id` values and seven variable values form one group; each receives the result
of its own session and variables, and `pg_stat_statements` counts exactly one statement per
run of the group, with the update as the only other statement. `<t>_by_pk` goes from null
to the inserted row and `<t>_aggregate` counts it. 101 subscriptions take two
statements per run; another role or another statement makes another group; `complete`
and a closed connection leave the group, and an empty group stops running. A role without
read permission has no subscription field. The multiplexed statement of each of the 43
admin and 19 permission fixtures, run for several sessions under custom and generic
plans, returns the result of each single statement, and every supported parameter type
keeps its value. A subscription whose statement fails, at subscription or later, gets
the error alone while its group keeps sending the other results.

The event tests in `crates/rusuna-events/tests/events.rs` each create their own database.
Inserts, updates and deletes, each once through a rusuna mutation and once in plain SQL,
reach the handler with the right `old` and `new`; a rolled-back insert records no event;
update columns and operations select the changes that record events. A non-superuser
applies the migration, a role with only `INSERT` on the table records an event without
reading `rusuna`, and a worker with the grants above delivers it. A handler that fails twice
gets its third attempt, each one at least the retry interval after the previous; a webhook
served by an axum server in the test answers 503 twice and 200 once and receives the
payload, the header and `delivery_info` each time. An error, a timeout and a panic each
fail an attempt; after the retry limit the event is `failed` and ten more polls try
nothing, until `redeliver`. Two workers deliver 200 events, each exactly once. A worker
aborted during a delivery leaves the event to another worker, which claims it only after
the lease has expired on an injected clock. Two workers record one event per cron
occurrence on an injected clock; a starting worker records the missed occurrences within
the tolerance and skips older ones; a scheduled event waits for its time and a
rolled-back one is not recorded. Shutdown waits for a delivery in progress; the migration
applies twice and replaces a changed trigger; `cleanup` keeps the latest cron
occurrence. Time-dependent tests move an injected clock and wait on the poll counters of
`Worker::monitor()`, never on a fixed sleep; the retry test only bounds the interval from
below. `crates/rusuna-cli/tests` compares the generated migration with
`crates/rusuna-cli/tests/fixtures/events.sql`.

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
Each test that loads its own tables creates its own database in the cluster.

## Publishing

`cargo package --workspace --exclude rusuna-cli --exclude rusuna-http-tests` builds the
six published crates; their dependencies on each other carry `version = "0.1.0"`, so
they are published in the order `rusuna`, `rusuna-http-client`, `rusuna-sqlx`, `rusuna-events`, `rusuna-async-graphql`,
`rusuna-axum`. The `rusuna` package contains `src/` and `.leanfran/cache/`, the Rust
leanfran generated from the `lean!` block. Its build script copies the cached module and
needs neither Lean nor Lake, so a crate that depends on the published `rusuna`
([leanfran](https://crates.io/crates/leanfran) and `leanfran-build` 0.6.0 come from
crates.io) builds with cargo and a C toolchain only. `lean/` is not packaged; the cache
key depends on the block and on the crate name, so a changed block needs a regenerated
cache before packaging, which `make check` enforces.

## License

Apache-2.0
