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
- `src/graphql.rs` and `src/graphql/`: SDL loading, deterministic expansion, typed GraphQL
  parsing with async-graphql-parser 7 and conversion to `Selection`.
- `crates/rusuna-cli/`: the clap CLI `rusuna`.
- `lean/` and `.leanfran/cache/`: the extracted Lean and the generated Rust. A build in
  the dev shell rewrites them when the block changes; commit them with the source.
- `audit/Audit/`: the leanassay audit `make check` runs. `audit/Survey/`: the tables of
  the functions outside the audit (`make survey`).
- `tests/fixtures/`: the schema, a small data set, the GraphQL queries, the statements
  Hasura v2.51.6 generates for them (`hasura/`, from `POST /v1/graphql/explain`) and the
  statements rusuna generates (`expected/`; rewrite them with
  `RUSUNA_UPDATE_EXPECTED=1`).
- `tests/common/mod.rs`: SDL schema loading and reference selections. Tests compare the
  parsed fixtures with those values; `bench/` compiles the fixture text.
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
and everything with I/O: tests, the database client and the benchmark. Rust also parses SDL and GraphQL, expands fragments and variables, and checks values
against the generated GraphQL type graph. SQL types and database execution remain caller
responsibilities. `Schema::from_sdl` builds the schema; `Schema::compile` accepts text,
JSON variables, an optional operation name and an explicit `Session`. `compile_selection` is the lower-level
entry to Lean. Keep selection acceptance rules and SQL generation in Lean.

The block has four parts.

- SQL tokens. A statement is a list of `Tok`: a `Lex` (raw text, an identifier, a
  qualified column, a number or a quoted string literal) or a parameter. `render` prints the tokens and numbers the
  parameters. Only a `Tok.param` carries a value of the query.
- The plan. `Query`, `Node` and `Predicate.exists` hold one `Scan` per table reference (GraphQL name, SQL table, recursive filter,
  order, limit and offset). `emit` turns a plan into tokens. Functions that return `List Lex` cannot
  emit a parameter; `expressionToks` emits filter values through `Tok.param`, including predicates in `EXISTS`.
- Resolution. `resolveSelection` builds the raw plan from a `Selection` and the role's
  visible schema. `resolve` inserts permissions and validates all scans and user column
  references. `run` is `resolve` followed by `compile`.
- Permissions. `Permission` holds a role, target, `Expression String Operand`, visible
  column fields and an aggregation flag. `Session` holds a role and scalar variables.
  `bindExpression` resolves `Operand.session` to a `Value` before normal expression
  resolution. The reserved `admin` role skips permission insertion. Rust calls the Lean
  `visibleSchema` to build the role's expanded SDL; permission decisions stay in Lean.

Names in a statement depend only on the nesting depth and the position of a field:
`r<depth>` for the rows of a level read as a set, `t<depth>` for a row of a level,
`c<index>` for the joined result of the field at `index`, `k<index>` for a key column and
`e<depth>` for an `EXISTS` row, and `%` for the row passed to `row_to_json`. A selection alias equal to `%` is rejected.

A relationship is read as a set (`isSet`) when its parent is read as a set, it is an
array or aggregate relationship and it has neither a limit nor an offset. The root is read as a set. Everything else is a
lateral subquery.

`maxDepth` (32) and `maxFields` (1000) bound a query. Aggregate selection wrappers
count toward nesting, with three extra wrapper levels. `maxFilterDepth` (32) bounds
filter edges, `maxFields` bounds each boolean expression, and `maxInValues` (1000)
bounds membership lists. The recursion over a plan is as
deep as the query and as long as a selection; `tests/compile.rs` compiles the largest
accepted query on a 2 MiB stack. The public limit constants in `src/lib.rs` repeat these numbers and a unit test
compares them. Rust also bounds filter conversion before descending recursively.
A combined test places the maximum filter inside the deepest accepted relationship.
`tests/permissions.rs` also checks maximum permission depth, boolean size and membership
size on a 2 MiB stack. A combined case has 32 relationship levels, 32 `EXISTS`
levels, a maximum-depth 1000-term permission and 65,001 parameters. Permission `limit` and session-valued `_is_null` are unsupported.

Do not silently apply another table's permission inside a permission relationship:
Hasura does not do so. `queryPermitted` requires every such nested scan to already contain
its role condition exactly, or as an immediate conjunction member. More general logical
implications are rejected. `allScans` includes these scans; `queryReads` excludes the
appended policy conjunct and join keys. `Scan` always wraps a user condition and policy
as `.and [user, policy]` after insertion. Keep this boundary consistent with `scanReads`.
Policy values follow the user filter values of each scan. The authorization proofs are
syntactic invariants of this plan; they do not establish PostgreSQL result semantics.

## Theorems and the audit

A theorem counts when it rules out a broken implementation. `audit/Audit/Compiler.lean`
and `audit/Audit/Permissions.lean` run [leanassay](https://github.com/conao3/lean-leanassay): `#assay` breaks each function in small ways and runs the theorems on the
broken versions. `lake -d audit build --wfail` fails when no broken version of a function
is refuted, when a broken version satisfies every theorem but changes the output (a
hole), or when no input satisfies a theorem's hypotheses.

The audit covers fourteen functions: 101 broken versions, 78 refuted, 16 with the same output
on every input tried, 7 that do not typecheck, no hole and no untested version.

- Properties: `quoteChars` and `quoteStringChars` (`unquote_quote`, `unquote_quoteChars`: reading a quoted name
  back gives the name; `unquote_quoteString` and `unquote_quoteStringChars` do the
  same for single-quoted literals), `assignStep`, `assign` and `tokValue` (`slots_numbered`: the
  placeholders are 1 to n in order) and `distinct` (`distinct_iff`: it accepts exactly
  the lists `List.Nodup` accepts).
- Conditions restated as an equivalence next to the definition: `aliasValid`,
  `selectionValid` and `relationValid` (`aliasValid_iff`, `selectionValid_iff`,
  `relationValid_iff`), `scalarValid` and `membershipValid` (`scalarValid_iff`,
  `membershipValid_iff`), and `lateralScan` (`lateralScan_limit`). The full scan equalities
  `lateralScan_object`, `lateralScan_array` and `lateralScan_aggregate` are proved too;
  the audit now checks the complete scan equalities as well as the limit projection.
  They stop an edit of one side only; they do not connect the definition to anything
  written independently.
- `appendCondition` and `addPermission` use `appendCondition_exact` and
  `addPermission_condition`: all three mutations are refuted, including returning the
  original scan without its condition. The table is in README.md. Audit-only default
  instances supply constant failing scans and errors to the mutator.

A function added to the block gets a theorem and an `#assay` line in the same change, or
an entry below. `maxDepth`, `maxFields`, `maxFilterDepth`, `maxInValues` and `rowAlias` are constants
and are not audited.

Three groups, targeting twenty functions, stay outside the audit. `make survey` prints their tables with hand-written
generators shared in `audit/Audit/Generators.lean`, since automatically derived generators
for recursive expressions and selections do not terminate. The library builds nine
independent survey modules so their automatically derived instances stay separate and
completed groups can be reused independently.

- `paramToks`, `expressionToks`, `expressionsToks`, `scanToks`, `joinToks`, `joinsToks`,
  `emit` and `compile` build the SQL
  text. `compile_params` and `placeholders_numbered` refute 26 of 295 broken versions,
  those that lose or reorder a parameter; `compile_erase` refutes none of the versions
  tried. The 172 holes change the text around the parameters, which no theorem
  describes. Four versions give the same output on the inputs tried and 93 do not
  typecheck; none is untested or times out. The differential test and
  `tests/fixtures/expected/` pin the text.
- `resolveExpression`, `resolveExpressions`, `resolveFilter`, `resolveOrder`,
  `resolveScan`, `resolveNode`, `resolveNodes`, `resolveSelection` and `runSelection` decide which selections are accepted. `resolveSelection_values` and `runSelection_params`
  refute 4 of 304 broken versions; `resolveSelection_declared` refutes none of the versions tried.
  The 100 holes accept or reject a different set of selections or return a different
  error. `invalid_queries_are_rejected` in `tests/compile.rs` pins the rules by example.
  126 versions do not typecheck and 47 give the same output on every input tried.
  None is untested. The filter survey has 27 unresolved timeouts at the default
  30-second deadline per mutation in this run with concurrent machine load; do not
  count these as equivalent or killed.
- `guardQuery`, `resolve` and `run` use `resolve_values` and `run_params`. The mutator
  generates one changed role comparison in `resolve`: zero killed, one hole, zero
  equivalent, ill-typed, untested or timed-out versions. It changes the accepted set
  while successful outputs still satisfy parameter preservation. No mutation is
  generated for `guardQuery` or `run` by this pinned revision.

leanassay compares outputs only for types with `DecidableEq`. The audit implements
`DecidableEq Predicate` from `predicateEq_iff` and a constructor comparison for `Scan`.
`Expression`, `Node`, `Query` and `Selection` have no instance; the surveys compare the
output of `runSelection` or `run` and declare `DecidableEq` for `Except`. The pinned
leanassay revision is unchanged. Hand-written generators remain necessary.

The lexical helpers `comparisonText`, `aggregateText`, `leafLex`, `offsetLex`,
`emptyLeaf`, `emptyField`, `emptyFields`, `emptyRow` and `resultLex` return text or
`List Lex`, so they cannot carry parameters. Their SQL semantics are covered by the
Hasura fixtures and their uses are covered by the emission survey. `findField`,
`relationMode`, `summaryOp`, `modeLeaf` and `childMode` implement field lookup and
aggregate selection contexts; their uses are covered by the resolution survey and
rejection tests. Value traversal, erasure and declared-reference helpers state and
support the preservation theorems; they are not independent acceptance checks.

The reported properties are nine independent properties, stated by fifteen boundary
theorems; fourteen conditions restated next to the implementation; and zero variable-free
checks. `PROOFS.md` lists every counted statement verbatim. Intermediate value,
declaration and permission traversal lemmas support those properties and are not counted
again. The new independent guarantees are each non-admin scan's role condition and the
visibility of user columns. `resolve_tables` follows from the scan condition guarantee.

The complete acceptance equivalence is unfinished. No independent Bool function yet
characterizes every selection accepted by `resolveSelection` and `resolve`. In particular,
`documentValues` uses raw resolution and is a parameter-order specification, not an
acceptance predicate. Changes in the accepted set or error variant can still be survey
holes. Keep this boundary explicit in public descriptions.

Manual mutation checks supplement automatic generation. Replacing the inserted
`.and [scan.conds, condition]` with `scan.conds` in `appendCondition` makes
`appendCondition_exact`, `addPermission_condition` and `addPermission_values` fail. Calling `roleCondition` with `{ session with role := "other" }`
inside `addPermission` fails `addPermission_condition` and `addPermission_values`. The pinned mutator does not generate
that role substitution; record the automatic table separately from these manual failures.
Earlier manual checks recorded in commit `1fe806c` made `condToks` emit a text value
and made `resolveScan` read another SQL schema; the erasure and raw declaration proofs
failed respectively. Those two historical mutations were not rerun for this change.
The new final validator can reject a corrupted scan before successful resolution.
Removing the column filter from `visibleSchema` fails `columnPermitted_origin`,
which derives original-table and original-permission witnesses for `resolve_columns`.
Manual proof failures are not extra automatic mutation kills.

`findPermission`, visibility, binding and guard traversal helpers implement the permissions;
the `addPermission` audit checks insertion, the permission survey checks the public chain,
and the differential/rejection tests cover their behavior. The syntactic equality and
scan/read traversal helpers state the invariants; they are not a second policy language.

When a proof is written, check it in a file of its own before building: wrap the
definitions in `namespace model` and run `lean` on the file. A build that fails in Lean
leaves `lean/` behind the source; delete `lean/LeanRustGen` and `.leanfran` and build
again.

## Tests

- `tests/differential.rs` parses all 43 admin GraphQL fixtures with the SDL schema and runs the statement of
  Hasura for it on the data of `tests/fixtures/seed.sql` and compares the JSON. Lists
  without `order_by` are compared as multisets. The seed has nullable text and floating-point columns, an author without articles,
  articles without comments, and a table with a two-column relationship. A new query gets its GraphQL text, the statement from
  Hasura's Explain API and a reference selection in `cases`.
- Another 19 differential fixtures use roles `user` and `reviewer`, two session values,
  restricted columns, and permissions on root, relationship, aggregate and `EXISTS`
  scans. Ten Hasura-rejected inputs are checked for rejection. Their SQL snapshots were
  compared with Hasura v2.51.6 HTTP responses; `tests/fixtures/permissions/metadata.json`
  records the metadata. The same PostgreSQL connection sets `hasura.user` before each
  Hasura statement, including statements that embed the captured session value directly.
- `tests/graphql.rs` checks SDL positions, typed inputs, fragment and variable expansion,
  input bounds and the committed expanded SDL. `crates/rusuna-cli/tests/` checks CLI
  output, help and exit codes. `make check` and the Nix package test the whole workspace.
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
