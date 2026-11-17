# rusuna

A Hasura-style GraphQL engine as a Rust library. rusuna compiles a GraphQL query over
tables and relationships into one PostgreSQL statement that returns the response as a
single JSON value. It runs inside your own process: no extra server, no database
extension.

The compiler core is written in Lean 4 with machine-checked theorems and is translated to
Rust by [leanfran](https://github.com/conao3/lean-leanfran).

## Status

rusuna compiles queries today. The schema and the query are passed as Rust values.

| Feature | State |
|---|---|
| Lists of rows with nested object and array relationships, `limit`, `order_by`, equality conditions | available |
| SDL with directives as the schema, the full `where` grammar, aggregates, a CLI | planned |
| Row and column permissions per role, with proofs | planned |
| Mutations in one transaction | planned |
| Parts for an `async-graphql` schema and `axum` routes | planned |
| Subscriptions, event triggers | planned |

## Usage

```rust
use rusuna::{Direction, Kind, Relation, Schema, Selection, Table};

let schema = Schema::new(vec![
    Table::new("author", "public", "author").column("id", "id").column("name", "name"),
    Table::new("article", "public", "article")
        .column("id", "id")
        .column("title", "title")
        .column("published_at", "published_at")
        .relation(Relation::new("author", Kind::Object, "author", ["author_id"], ["id"]))
        .relation(Relation::new("comments", Kind::Array, "comment", ["id"], ["article_id"])),
    Table::new("comment", "public", "comment").column("id", "id").column("body", "body"),
]);

// query { article(limit: 20, order_by: {published_at: desc}) {
//   id title author { name } comments(limit: 5, order_by: {id: asc}) { body } } }
let query = Selection::new("article")
    .limit(20)
    .order_by("published_at", Direction::Desc)
    .field(Selection::new("id"))
    .field(Selection::new("title"))
    .field(Selection::new("author").field(Selection::new("name")))
    .field(
        Selection::new("comments")
            .limit(5)
            .order_by("id", Direction::Asc)
            .field(Selection::new("body")),
    );

let compiled = schema.compile(&query)?;
// compiled.sql: one statement returning one `json` value, the list of rows
// compiled.params: the values of the query, for $1, $2, …
```

Run `compiled.sql` with any PostgreSQL client and put the returned JSON under the name of
the root field in the `data` of the response. `tests/differential.rs` does this with the
`postgres` crate.

## The generated SQL

- One statement per root field. PostgreSQL builds the JSON with `row_to_json` and
  `json_agg`.
- An object relationship and an array relationship with a `limit` are read by a
  `LEFT JOIN LATERAL` subquery per parent row.
- An array relationship without a `limit` whose parents are read as a set is read once:
  its rows are restricted to the parent rows with `IN`, aggregated by the parent key, and
  joined to the parents.
- Every value of a query is a statement parameter. `limit` is a non-negative integer
  written into the statement, since PostgreSQL chooses the plan by it.
- Every identifier is double-quoted, with the quotes inside it doubled.

`tests/fixtures/expected/` holds the statements for the queries of
`tests/fixtures/queries/`.

## What is proved

The Lean definitions and theorems are in `src/compiler.rs`. Each theorem holds for every
schema and every query.

Properties:

| Theorem | Statement |
|---|---|
| `run_params`, `compile_params`, `resolve_values` | The parameters of a compiled query are the values of the query, in document order. |
| `compile_erase` | The SQL text is the same for two queries that differ only in their values. |
| `placeholders_numbered`, `slots_numbered` | The placeholders are `$1` … `$n`, in the order of the parameters. |
| `unquote_quote`, `unquote_quoteChars` | `unquoteChars`, a reader of a double-quoted identifier written after PostgreSQL's lexical rules, reads a quoted name back as the name and stops at its closing quote. |
| `resolve_declared` | Every table in the plan of a compiled query is a table of the schema. |
| `distinct_iff` | The check for repeated aliases accepts exactly the lists without duplicates. |

Conditions written a second time, as an equivalence next to the definition:

| Theorem | Statement |
|---|---|
| `aliasValid_iff`, `selectionValid_iff` | An accepted selection is not empty, has at most 1000 fields, and its aliases are distinct, not empty, not `%` and at most 63 bytes long. |
| `relationValid_iff` | An accepted relationship has as many columns as references, and at least one. |
| `lateralScan_object`, `lateralScan_array` | The scan of an object relationship has the limit 1 and the conditions of the relationship; the scan of an array relationship is unchanged. |

[leanassay](https://github.com/conao3/lean-leanassay) checks which theorems rule out
broken implementations. AGENTS.md lists the functions it covers and the ones it does not.

Outside the proofs:

- What a statement returns. The theorems are about the text and the parameters of a
  statement, not about its result in PostgreSQL. The differential test compares the
  results with those of the statements Hasura generates.
- The acceptance rules of `resolve`, apart from the conditions above. Tests cover them by
  example.
- leanfran, which translates the Lean definitions to the Rust that runs. The tests run
  the generated Rust.
- `src/lib.rs`, which converts the public Rust types to the Lean types and back.
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
| A: 20 articles, their author and 5 comments each | 0.6 (0.5–2.6) | 0.9 (0.5–2.0) | 0.6 (0.5–1.5) | 0.6 (0.5–3.0) |
| B: 50,000 articles and their author | 131.7 (121.7–192.2) | 200.7 (183.2–234.0) | 138.1 (124.3–180.7) | 180.5 (158.4–239.1) |
| C: 2,000 authors, all their articles and all their comments | 679.0 (589.0–830.5) | 721.3 (630.1–767.3) | 714.9 (644.3–821.9) | 710.0 (635.0–810.5) |

Compiling one of these queries takes 50 to 92 µs.

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
