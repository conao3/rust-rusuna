# rusuna development

Rust edition 2024, leanfran, flake-parts and treefmt-nix. Lean and Lake are fixed to
4.35.0, leanfran to the revision in `flake.nix` and the crates `=0.6.0`. Keep code,
tests, commit messages and documentation in English. Write code without comments,
trailing spaces or alignment padding.

Run `nix develop --command make check` and `nix build` before committing. Add new files
to Git before using Nix. CI runs the same two commands on pushes to master and on pull
requests. Keep action version tags in `.github/workflows/ci.yml`, update their commits
with `gh actions-lock --no-interactive`, verify with `gh actions-lock --verify`, and
commit the workflow and `actions.lock` together.

## Layout

- `src/compiler.rs`: the `lean!` block `model`, the whole compiler core.
- `src/lib.rs`: the public Rust types and their conversion to and from `model`.
- `lean/` and `.leanfran/cache/`: the extracted Lean and the generated Rust. A build in
  the dev shell rewrites them when the block changes; commit them with the source.
- `audit/Audit/`: the leanassay audit `make check` runs. `audit/Survey/`: the tables of
  the functions outside the audit (`make survey`).
- `tests/fixtures/`: the schema, a small data set, the GraphQL queries, the statements
  Hasura v2.51.6 generates for them (`hasura/`, from `POST /v1/graphql/explain`) and the
  statements rusuna generates (`expected/`; rewrite them with
  `RUSUNA_UPDATE_EXPECTED=1`).
- `tests/common/mod.rs`: the schema and the queries as Rust values, shared with `bench/`.
- `bench/`: a crate outside the workspace that measures the statements on the full data
  set (`make bench`).
- `tools/with-postgres.sh`: runs a command with a throwaway PostgreSQL cluster and
  `RUSUNA_POSTGRES` set.

## Lean and Rust

Lean owns everything between a schema with a selection and the SQL text with its
parameters: resolving field names against the schema, validating the selection, the query
plan, the SQL tokens, quoting identifiers and numbering placeholders. Rust calls the
generated `run` and converts its arguments and result. Do not reimplement these in Rust.

Rust owns the public types, the depth check that bounds the recursion of the conversion,
and everything with I/O: tests, the database client and the benchmark. Parsing GraphQL
and SDL, executing statements and the server integrations will be Rust as well.

The block has three parts.

- SQL tokens. A statement is a list of `Tok`: a `Lex` (raw text, an identifier, a
  qualified column, a number) or a parameter. `render` prints the tokens and numbers the
  parameters. Only a `Tok.param` carries a value of the query.
- The plan. `Query` and `Node` hold one `Scan` per table reference (table, conditions,
  order, limit). `emit` turns a plan into tokens. Functions that return `List Lex` cannot
  emit a parameter; `scanToks` is the only place a condition becomes tokens.
- Resolution. `resolve` checks a `Selection` against a `Schema` and builds the plan;
  `run` is `resolve` followed by `compile`.

Names in a statement depend only on the nesting depth and the position of a field:
`r<depth>` for the rows of a level read as a set, `t<depth>` for a row of a level,
`c<index>` for the joined result of the field at `index`, `k<index>` for a key column and
`%` for the row passed to `row_to_json`. A selection alias equal to `%` is rejected.

A relationship is read as a set (`isSet`) when its parent is read as a set, it is an
array relationship and it has no limit. The root is read as a set. Everything else is a
lateral subquery.

`maxDepth` (32) and `maxFields` (1000) bound a query. The recursion over a plan is as
deep as the query and as long as a selection; `tests/compile.rs` compiles the largest
accepted query on a 2 MiB stack. `MAX_DEPTH` and `MAX_FIELDS` in `src/lib.rs` repeat the
two numbers and a unit test compares them.

## Theorems and the audit

A theorem counts when it rules out a broken implementation. `audit/Audit/Compiler.lean`
runs [leanassay](https://github.com/conao3/lean-leanassay): `#assay` breaks each function in small ways and runs the theorems on the
broken versions. `lake -d audit build --wfail` fails when no broken version of a function
is refuted, when a broken version satisfies every theorem but changes the output (a
hole), or when no input satisfies a theorem's hypotheses.

The audit covers nine functions: 62 broken versions, 54 refuted, 4 with the same output on
every input tried, 4 that do not typecheck, no hole.

- Properties: `quoteChars` (`unquote_quote`, `unquote_quoteChars`: reading a quoted name
  back gives the name), `assignStep`, `assign` and `tokValue` (`slots_numbered`: the
  placeholders are 1 to n in order) and `distinct` (`distinct_iff`: it accepts exactly
  the lists `List.Nodup` accepts).
- Conditions restated as an equivalence next to the definition: `aliasValid`,
  `selectionValid` and `relationValid` (`aliasValid_iff`, `selectionValid_iff`,
  `relationValid_iff`) and `lateralScan` (`lateralScan_object`, `lateralScan_array`).
  They stop an edit of one side only; they do not connect the definition to anything
  written independently.

A function added to the block gets a theorem and an `#assay` line in the same change, or
an entry below. `maxDepth`, `maxFields` and `rowAlias` are constants and are not audited.

Two groups stay outside the audit. `make survey` prints their tables with hand-written
generators, since the generators leanassay derives for `Node` and `Selection` do not
terminate.

- `condToks`, `scanToks`, `joinToks`, `joinsToks`, `emit` and `compile` build the SQL
  text. `compile_params` and `placeholders_numbered` refute 15 of 139 broken versions,
  those that lose or reorder a parameter; `compile_erase` refutes none of the versions
  tried. The 120 holes change the text around the parameters, which no theorem
  describes. The differential test and `tests/fixtures/expected/` pin the text.
- `resolveConds`, `resolveOrder`, `resolveScan`, `resolveNode`, `resolveNodes`, `resolve`
  and `run` decide which selections are accepted. `resolve_values` and `run_params`
  refute 5 of 85 broken versions; `resolve_declared` refutes none of the versions tried.
  The 38 holes accept or reject a different set of selections or return a different
  error. `invalid_queries_are_rejected` in `tests/compile.rs` pins the rules by example.
  33 versions do not typecheck and 9 give the same output on every input tried.

leanassay compares outputs only for types with `DecidableEq`. `Node`, `Query` and
`Selection` have none (the deriving handler does not apply to nested inductive types), so
the survey compares the output of `run` and declares `DecidableEq` for `Except`.

When a proof is written, check it in a file of its own before building: wrap the
definitions in `namespace model` and run `lean` on the file. A build that fails in Lean
leaves `lean/` behind the source; delete `lean/LeanRustGen` and `.leanfran` and build
again.

## Tests

- `tests/differential.rs` runs each query of `tests/common/mod.rs` and the statement of
  Hasura for it on the data of `tests/fixtures/seed.sql` and compares the JSON. Lists
  without `order_by` are compared as multisets. The seed has an author without articles
  and articles without comments. A new query gets its GraphQL text, the statement from
  Hasura's Explain API and an entry in `cases`.
- `tests/compile.rs` holds the expected statements, the parameters and their order, the
  rejected selections, the largest query on a small stack and compiling from several
  threads.
- `RUSUNA_POSTGRES` is a required input of the differential test and the benchmark;
  `tools/with-postgres.sh` sets it.

## Speed

The goals on the data of `bench/seed.sql`, measured by `make bench`: for the queries A
and B the median is inside the range of the Hasura statement in the same run, and for C
it is at most 1.5 times the median of the hand-written statement. Measure a change of the
SQL shape with all four statements in the same run. The shapes were chosen by
measurement:

- `row_to_json` over a subselect is faster than `json_build_object` (B: 114 ms against
  150 ms).
- A `limit` written as `$1` makes PostgreSQL plan A for an unknown number of rows under a
  generic plan (147 ms against 1.1 ms with `plan_cache_mode = force_generic_plan`).
- Reading an unlimited array relationship as a set is as fast as the hand-written
  statement of C; the lateral form of the Hasura statement took about 1.3 times as
  long in `psql`.
