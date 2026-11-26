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

Release with `gh workflow run release.yml --ref master -f version_bump=patch` (`minor` when
the public types gain fields or variants). The workflow bumps the version, checks, pushes the
version commit and the `v<version>` tag, publishes the 6 crates, and creates the GitHub
Release; don't push release tags by hand.

## Layout

- `src/compiler.rs`: the `lean!` block `model`, the whole compiler core.
- `src/lib.rs`: the public Rust types and their conversion to and from `model`.
- `src/statement.rs`: the reader of native query statements: `{{name}}` markers become
  `SqlPart::Argument`, and text the subscription multiplexer cannot read is rejected.
- `src/graphql.rs` and `src/graphql/`: SDL loading, deterministic expansion, typed GraphQL
  parsing with async-graphql-parser 7 and conversion to `Selection`.
- `crates/rusuna-cli/`: the clap CLI `rusuna` (not published): `schema expand-sdl`,
  `query compile-sql` and `events generate-migration`.
- `crates/rusuna-sqlx/`: `fetch` and `execute` with sqlx 0.9. Parameters are encoded
  for the types PostgreSQL infers when sqlx prepares the statement. `src/subscription.rs`
  holds `Multiplexed`, the statement that runs one compiled statement for many parameter
  lists, and `Subscriptions`, the poller that groups subscriptions by role and SQL text.
- `crates/rusuna-async-graphql/`: `Builder` and `Parts`, the async-graphql 7 dynamic
  schema of one role, the root resolvers (query, mutation and subscription), hand-written
  fields and the transaction extension. `src/remote.rs` holds `RemoteSchema`: the SDL or
  introspection import, the type and root field prefixes, the role list and the resolver
  that forwards a remote root field, rendered with the same `Document` as rusuna root
  fields from its own `Catalog` of argument types and child types.
- `crates/rusuna-axum/`: the axum 0.8 routes `graphql` (`POST`) and `graphql_ws`
  (WebSocket, `graphql-transport-ws`), `src/rest.rs` with `rest`, the REST endpoints
  (validated per role in introspection-only mode when they are built), and the example
  `blog`, which also serves a REST endpoint and runs an event worker.
- `crates/rusuna-events/`: event triggers, cron triggers and scheduled events, all in
  Rust; the Lean core is not involved. `src/migration.rs` generates the idempotent
  migration (tables `rusuna.event_log` and `rusuna.event_invocation`, the function
  `rusuna.capture_event()` and one `rusuna_event_<name>` trigger per declaration),
  `src/worker.rs` holds `Events` and `Worker` (claims with `FOR UPDATE SKIP LOCKED`, a
  lease, results fenced by `tries`, `LISTEN rusuna_events` beside polling),
  `src/cron.rs` the five-field cron triggers (croner 4) and `src/webhook.rs` the webhooks,
  sent with `rusuna-http-client`. Every worker query takes the time from the injected
  `Clock`.
- `crates/rusuna-http-client/`: `Client::post_json`, hyper 1 with rustls on ring and the
  webpki roots, shared by the webhooks and remote schemas so neither crate depends on the
  other.
- `crates/rusuna-http-tests/`: unpublished; HTTP tests against the Hasura fixtures and
  the introspection comparison. Each test creates its own database.
- Workspace dependencies and package metadata are in the root `Cargo.toml`
  (`[workspace.dependencies]`, `[workspace.package]`).
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
  `RUSUNA_POSTGRES` set. The cluster preloads `pg_stat_statements`, which the subscription
  tests use to count statements.

## Lean and Rust

Lean owns everything between a schema with a selection and the SQL text with its
parameters: resolving field names against the schema, validating the selection, the query
plan, the SQL tokens, quoting identifiers and numbering placeholders. Rust calls the
generated `run` and converts its arguments and result. Do not reimplement these in Rust.
The multiplexed subscription statement in `rusuna-sqlx` replaces the placeholders of a
compiled statement; it reads the numbering Lean produces and fails unless the placeholders
outside quoted text are `$1` to `$n` in order.

Rust owns the public types, the depth check that bounds the recursion of the conversion,
and everything with I/O: tests, the database client and the benchmark. Rust also parses SDL and GraphQL, expands fragments and variables, and checks values
against the generated GraphQL type graph. The `rusuna` crate performs no I/O; the other
crates execute statements, build the async-graphql schema, serve HTTP and deliver events. SQL column
types remain the caller's database schema.

The async-graphql resolvers hand a root field to the core as GraphQL text: the alias, the
selection from `Context::field()`, and every argument as a variable declared with the
argument's type from the role's expanded SDL, its value passed as JSON. Argument values
are never printed as GraphQL literals: enum values arrive from clients as JSON strings,
which only the core's variable coercion accepts. async-graphql removes `@skip` and `@include` before resolvers run;
the extension's `prepare_request` fills variable defaults and omitted nullable variables
first. A subscription root field is rendered as a `subscription` document, which the core
compiles with the query root's fields to the statement of the same query. Selection rules stay in the core. `Schema::from_sdl` builds the schema; `Schema::compile` accepts text,
JSON variables, an optional operation name and an explicit `Session`. `compile_selection` is the lower-level
entry to Lean. Keep selection acceptance rules and SQL generation in Lean.

The block has four parts.

- SQL tokens. A statement is a list of `Tok`: a `Lex` (raw text, an identifier, a
  qualified column, a number or a quoted string literal) or a parameter. `render` prints the tokens and numbers the
  parameters. Only a `Tok.param` carries a value of the query.
- The plan. `Query`, `Node` and `Predicate.exists` hold one `Scan` per table reference (GraphQL name,
  `Source`, recursive filter, order, limit and offset). A `Source` is a SQL table or the tokens of
  a native query statement, whose arguments are `Tok.param`. `emit` turns a plan into tokens. Functions that return `List Lex` cannot
  emit a parameter; `expressionToks` emits filter values through `Tok.param`, including predicates in `EXISTS`.
- Resolution. `resolveSelection` builds the raw plan from a `Selection` and the role's
  visible schema. A `Table` has a `Body`: `relation` (its SQL table), `statement`
  (the parts of a native query and its argument names) or `function` (a schema, a function
  name and its argument names, called with named arguments). A `RootField` may carry a body
  that replaces its target's for the root scan (`SETOF` functions); `tableBodies` lists the
  bodies a scan of a type may read, and `scanPermitted` checks the source against them. `resolveSource` fills a statement with
  the arguments of the root selection; every other selection must have no arguments, so
  relationship and `EXISTS` scans read only statements without arguments. `rootValues`
  gives the root's argument values in the order of the markers. `resolve` inserts permissions and validates all scans and user column
  references. `run` is `resolve` followed by `compile`.
- Permissions. `Permission` holds a role, target, `Expression String Operand`, visible
  column fields, an aggregation flag and an optional row limit. `guardNode` and `guardQuery`
  apply the limit where the kind of a level is known (`listScan` for list roots and array
  relationships, `listChildren` for the `nodes` of aggregates); `guardScan`, `addPermission`
  and `EXISTS` scans do not see it. The by-primary-key root has the root kind `single`, which
  resolves to an object-kind query and takes no limit. `Session` holds a role and scalar variables.
  `bindExpression` resolves `Operand.session` to a `Value` before normal expression
  resolution. The reserved `admin` role skips permission insertion. Rust calls the Lean
  `visibleSchema` to build the role's expanded SDL; permission decisions stay in Lean.

Names in a statement depend only on the nesting depth and the position of a field:
`r<depth>` for the rows of a level read as a set, `t<depth>` for a row of a level,
`c<index>` for the joined result of the field at `index`, `k<index>` for a key column and
`e<depth>` for an `EXISTS` row, and `%` for the row passed to `row_to_json`. A selection alias equal to `%` is rejected.

A relationship is read as a set (`isSet`) when its parent is read as a set, it is an
array or aggregate relationship and it has neither a limit nor an offset. A permission limit
sets the limit of an array relationship, so a limited role reads it laterally. The root is read as a set. Everything else is a
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
levels, a maximum-depth 1000-term permission and 65,001 parameters. Session-valued `_is_null` is unsupported.

Do not silently apply another table's permission inside a permission relationship:
Hasura does not do so. `queryPermitted` requires every such nested scan to already contain
its role condition exactly, or as an immediate conjunction member. More general logical
implications are rejected. `allScans` includes these scans; `queryReads` excludes the
appended policy conjunct and join keys. `Scan` always wraps a user condition and policy
as `.and [user, policy]` after insertion. Keep this boundary consistent with `scanReads`.
Policy values follow the user filter values of each scan. The authorization proofs are
syntactic invariants of this plan; they do not establish PostgreSQL result semantics.

## Theorems and the audit

A theorem counts when it rules out a broken implementation. `audit/Audit/Compiler.lean`,
`audit/Audit/Permissions.lean` and `audit/Audit/Acceptance.lean` run [leanassay](https://github.com/conao3/lean-leanassay): `#assay` breaks each function in small ways and runs the theorems on the
broken versions. `lake -d audit build --wfail` fails when no broken version of a function
is refuted, when a broken version satisfies every theorem but changes the output (a
hole), or when no input satisfies a theorem's hypotheses.

The audit covers forty-two functions: 266 broken versions, 182 refuted, 23 with the same output
on every input tried, 61 that do not typecheck, no hole, no untested version and no timeout.

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
  the audit checks the complete scan equalities as well as the limit projection.
  They stop an edit of one side only; they do not connect the definition to anything
  written independently.
- `sourceToks` uses `sourceToks_table` and `sourceToks_tokens`; `argumentValid` and
  `acceptsArguments` use `argumentValid_iff` and `acceptsArguments_iff`; `bodySource`,
  `partToks`, `templateToks` and `partShape` use `bodySource_values` and `bodySource_fits`;
  `namedToks` and `callToks` use `namedToks_pair`, `namedToks_single` and `callToks_exact`;
  `sourceFits` uses `sourceFits_iff`; `sourceDeclared` and `tableBodies` use
  `sourceDeclared_iff` and `tableBodies_iff`; `resolveSource` uses `resolveSource_exact`.
  `Audit.AcceptanceGenerators` gives standalone tables random bodies so that these reach
  statements.
- `capLimit` uses `capLimit_cap`, `capLimit_user` and `capLimit_choice` (at most the
  permission's limit, at most the query's, and one of the two). `limitScan`, `listScan`,
  `limitNodes` and `listChildren` use `limitScan_exact`, `listScan_array`, `listScan_other`,
  `limitNodes_limit`, `listChildren_limits` and `listChildren_other`; every mutation that drops
  the bound is refuted. `resolve_limits` follows from the final validator `queryLimited` in
  `resolve`.
- `appendCondition` and `addPermission` use `appendCondition_exact` and
  `addPermission_condition`: all three mutations are refuted, including returning the
  original scan without its condition. The table is in README.md. Audit-only default
  instances supply constant failing scans and errors to the mutator.
- `resolveExpressions`, `resolveFilter`, `resolveOrder`, `resolveScan`,
  `resolveNodes`, `resolveSelection` and `runSelection` use the Boolean acceptance
  equalities. An audit-only `Inhabited (Except Error Compiled)`
  supplies an error result so the `runSelection` audit checks unconditional rejection.

A function added to the block gets a theorem and an `#assay` line in the same change, or
an entry below. `maxDepth`, `maxFields`, `maxFilterDepth`, `maxInValues` and `rowAlias` are constants
and are not audited.

Five groups, targeting eighteen functions, stay outside the audit. `make survey` prints their tables with hand-written
generators shared in `audit/Audit/Generators.lean`. These choose names from a small
vocabulary so the sampled schemas and selections exercise successful resolution.
The pinned leanassay revision also supports size-bounded generators for recursive types. The library builds eight
independent survey modules beside `Survey.Compiler`, which imports the shared
generators, so their automatically derived instances stay separate and
completed groups can be reused independently.

| Group | Functions | Mutations | Killed | Holes | Equivalent on sampled inputs | Untested | Timed out | Stillborn |
|---|---|---|---|---|---|---|---|---|
| SQL emission | 8 | 180 | 26 | 101 | 2 | 0 | 0 | 51 |
| Selection resolution | 2 | 139 | 40 | 13 | 30 | 0 | 0 | 56 |
| Permission resolution | 3 | 2 | 0 | 1 | 1 | 0 | 0 | 0 |
| Write emission | 2 | 87 | 0 | 73 | 10 | 0 | 0 | 4 |
| Write resolution | 3 | 57 | 1 | 20 | 32 | 0 | 0 | 4 |
| Total | 18 | 465 | 67 | 208 | 75 | 0 | 0 | 115 |

- The SQL emission group is `paramToks`, `expressionToks`, `expressionsToks`,
  `scanToks`, `joinToks`, `joinsToks`, `emit` and `compile`. Parameter preservation,
  erasure and placeholder numbering constrain values. SQL text is pinned by the
  differential test and `tests/fixtures/expected/`.
- The selection group is `resolveExpression` and `resolveNode`. Success equivalences use `acceptsSelection` and
  its traversal helpers alongside the value and declaration theorems.
  `resolveExpression` has two holes (the `_and` and `_or` size checks) and `resolveNode`
  eleven (the conjunctions of the argument checks and the choice between `nodes` and a group):
  they change errors or node constructors without changing success or parameters on the
  sampled inputs. These require output properties beyond success equivalence. No
  mutation timed out.
  Rejection and differential tests exercise the input rules and emitted statements.
- `guardQuery`, `resolve` and `run` use `resolve_values` and `run_params`. The changed
  role comparison in `resolve` changes acceptance while preserving successful
  outputs' parameters.
- The write emission group is `writeBodyToks` and `emitWrite` with `emitWrite_slots`,
  which fixes placeholder numbering only. No write statement corresponds to
  `compile_params`, so its 73 holes change SQL text or drop and reorder parameters; the
  committed SQL fixtures, the Hasura comparison and the concurrent-transaction tests
  pin both. The
  `WritePlan` generator mixes primary-key lists with returning relationships that
  read the modified table again.
- The write resolution group is `resolveWriteRaw`, `resolveWrite` and `runWrite` with
  `resolveWrite_secure` and `runWrite_accepts`. Its 20 holes in `resolveWriteRaw` and
  `resolveWrite` change which inputs are accepted, including a constant error; the
  statements constrain successful plans, not write acceptance.

The survey uses 1000 samples and a 30-second deadline per mutation. Timeouts are
unresolved measurements under concurrent machine load.

leanassay compares outputs only for types with `DecidableEq`. The audit implements
`DecidableEq Predicate` from `predicateEq_iff` and a constructor comparison for `Scan`.
`Expression`, `Node`, `Query` and `Selection` have no instance. The surveys use
predicate and scan comparisons where available and the output of `runSelection`
or `run` for opaque plans, with `DecidableEq` for `Except`. Output comparisons without a usable equality instance are counted as untested.
The hand-written generators retain the small name vocabulary.
`Audit.AcceptanceGenerators` uses the existing `genTable` for standalone `Table`
arguments of the acceptance lemmas; schema and selection generators are shared
unchanged with the emission and permission surveys.

The lexical helpers `comparisonText`, `aggregateText`, `leafLex`, `offsetLex`,
`emptyLeaf`, `emptyField`, `emptyFields`, `emptyRow` and `resultLex` return text or
`List Lex`, so they cannot carry parameters. Their SQL semantics are covered by the
Hasura fixtures and their uses are covered by the emission survey. `findField`,
`relationMode`, `summaryOp`, `modeLeaf` and `childMode` implement field lookup and
aggregate selection contexts; their uses are covered by the resolution survey and
rejection tests. Value traversal, erasure and declared-reference helpers state and
support the preservation theorems; they are not independent acceptance checks.

The reported properties are thirteen independent properties, stated by twenty-four boundary
theorems; thirty-six boundary conditions restated next to the implementation; and zero variable-free
checks. `PROOFS.md` lists every counted statement verbatim. Intermediate value,
declaration and permission traversal lemmas support those properties and are not counted
again. The permission guarantees are each non-admin scan's role condition and the
visibility of user columns. `resolve_tables` follows from the scan condition guarantee.

`acceptsSelection` and its expression, ordering, scan and node helpers describe
accepted pre-permission inputs without calling a resolver or constructing a plan.
The acceptance equalities for `resolveSelection` and `runSelection` are the two
counted boundary statements; the traversal lemmas and existential success form
support those same conditions. These eight Boolean functions are specifications
outside the compiler call graph; the audit mutates their resolvers against them.

The role-aware acceptance equivalence for `resolve` and `run` is pending. The
missing lemma characterizes `guardQuery` followed by `queryPermitted` from the raw
selection, including session binding and syntactic dependencies between permissions.
Individual role conditions can resolve while a relationship inside one condition
fails to contain its target's role condition. PROOFS.md describes this boundary.
`documentValues` is a parameter-order specification using raw resolution. Error
variants and SQL text require properties beyond success equivalence.

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
`permissionLimit`, `nodesLimit`, `listLimits`, `nodeLimits`, `nodesLimits`, `queryLimits`,
`withinLimit` and `queryLimited` state and check the row-limit property; the limit fixtures
exercise them.
`sourceValues`, `eraseSource`, `bodyParameters`, `partValid`, `bodyValid`, `argumentValues`,
`rootValues` and `scanDeclared` state the values and declarations of sources for the value
and declaration theorems; the native query tests exercise them.

When a proof is written, check it in a file of its own before building: wrap the
definitions in `namespace model` and run `lean` on the file. A build that fails in Lean
leaves `lean/` behind the source; delete `lean/LeanRustGen` and `.leanfran` and build
again.

## Write plans

`MutationArguments` and `OnConflict` carry typed input data to `runWrite`. The
`WriteCatalog` holds primary keys and operation-specific permissions independently of
the read schema. Native query and function types are read-only tables, and `resolveWriteRaw` also rejects a
target whose body is not a table. Read-only tables (`@table(read_only: true)`, views) are a Rust check:
the SDL emits no mutation roots for them, `Schema::new` rejects their write permissions
and `compile_mutation` rejects them as targets before calling Lean. SDL declares `@primary_key`, `@insert_permission`,
`@update_permission` and `@delete_permission`. The parser uses the expanded mutation
input types, and Lean resolves columns, presets, sessions, filters and returning nodes.

`resolveWrite` validates a `WritePlan`. Its check is `WriteCheck.abort` for permitted
inserts and updates. Updates and deletes have the operation's row condition as a
conjunct. The plan's assignments must map to permitted columns or operation presets;
user-provided columns cannot override presets. `ConflictPlan` uses the update
permission independently, including its condition, presets and aborting check.
Conflict updates with primary-key presets are rejected because the returning snapshot
requires stable conflict keys. Ordinary updates can change keys.

`applyWriteCondition`, `applyWriteCheck` and `applyWritePermission` are in
`audit/Audit/Writes.lean` under `--wfail`: ten mutations, all killed. The first two
exact equalities and the insertion function's two projections make omissions fail even
when the final validator could otherwise turn the corrupted plan into an error.
`resolveWrite_permissions`, `resolveWrite_user_columns`, `resolveWrite_declared`,
`resolveWrite_tables` and `resolveWrite_returning_conditions` are the counted boundaries.
Table-permission existence follows from the same operation-permission witness and is
not an additional independent property. `PROOFS.md` states the
scope and copied theorem statements.

Each mutation emits one statement: a data-modifying `m0` CTE, a changed-row `r0` CTE,
a post-operation `w0` CTE and the JSON result. `RETURNING` emits a composite row and
an original tuple identifier, keeping internal check columns out of the row data.
When `returning` reads the modified table again, updates and deletes join the
snapshot row by the declared primary-key columns (`ctid` for an update of a table
without one) to return its original `ctid`; other deletes return that identifier
directly. Returning relationships of the modified target use `w0`, so cycles
and aggregates observe changed rows. Insert conflicts exclude existing snapshot rows
by their declared primary key. Aliases are fixed by depth and position. Query emission
keeps its existing SQL snapshots byte-for-byte.

The top-level returning CTE has column permissions and no read row condition.
`resolveReturning` and `writeOutputsPermitted` check relationships and their predicate
scans with the existing `scanPermitted` and `columnPermitted` functions. Source rewriting
for the post-operation snapshot happens during SQL emission; its database semantics
are covered by differential tests rather than the scan-condition theorem.

The write survey targets `writeBodyToks`, `emitWrite`, `resolveWriteRaw`, `resolveWrite`
and `runWrite`. `emitWrite_slots` specializes placeholder numbering; `resolveWrite_secure`
checks the final authorization validators; `runWrite_accepts` compares success with
resolution. These are supporting statements rather than independent acceptance or SQL
semantics theorems.

The helper functions outside strict auditing are grouped by their use:

- `sourceLex`, `tokenCommas`, `writeCheckToks`, `assignmentToks`, `conflictToks`,
  `conflictCheckToks`, `snapshotJoinLex`, `returningToks` and `writeOutputToks` lower
  statements. The emission survey and committed SQL and database-result fixtures cover their uses.
- `snapshotPredicate`, `snapshotPredicates`, `snapshotScan`, `snapshotNode` and
  `snapshotNodes` redirect returning references. Cyclic relationship fixtures exercise
  insert, update, delete and upsert.
- `findWritePermission`, `writeVisible`, `resolveAssignments`, `bindPresets`,
  `writePolicy`, `resolveReturning`, `resolveWriteOutputs`, `keyCondition`,
  `keyColumns`, `resolveWriteRows`, `resolveConflict` and `runPrimaryKey` resolve inputs. Their uses
  are exercised by the write-resolution survey and the differential/rejection tests.
- `writeColumns`, `userWritesAllowed`, `writeColumnAllowed`, `writePermitted`,
  `returningPermitted`, `conflictAsWrite`, `userInputPermitted`, `writeOutputsPermitted`
  and `conflictPermitted` and `writeDeclared` define the invariants used by the permission proofs. They are
  not independent specifications of SQL execution or selection acceptance.

The public error recognizer checks SQLSTATE `22P02` and the permission-check marker.
Callers execute root statements in order inside one transaction and roll back on failure.
Mutation fixtures store the actual Hasura v2.51.6 HTTP response, role, session, query
and table state. The PostgreSQL tests reset data and identity sequences per fixture;
checks run under both custom and generic plans, and single-statement permission failures
are also tested in autocommit mode without a caller rollback.

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
- `crates/rusuna-http-tests/` runs an axum server per test on `127.0.0.1:0` and posts
  with a raw HTTP/1.1 client. It compares the 43 admin and 19 permission queries with the
  Hasura statements, the 64 mutation fixtures with Hasura's responses and tables, a
  rollback after a failing later root field, enum inputs as variables, repeated mutation
  response keys, hand-written fields in both directions,
  hidden roles and introspection with the expanded SDL of each role. Pruned empty types
  are listed by hand in `tests/introspection.rs`.
- `tests/natives.rs` compares native query roots (`tests/fixtures/natives.graphql`) with
  hand-written statements on the seed: arguments, filters, ordering, pagination, aggregates,
  relationships in both directions, `EXISTS`, permissions, parameter order and rejections.
  `crates/rusuna-http-tests/tests/natives.rs` repeats them over `POST` with variables, for
  every scalar argument type and null, and as a REST endpoint; `subscriptions.rs` runs the
  multiplexed statement of a native query whose text contains `'$1'` and a live subscription.
- `tests/functions.rs` compares function roots (`tests/fixtures/functions.graphql`, created by
  `tests/fixtures/functions.sql`) with statements that call the functions directly: a `SETOF`
  table function with relationships, aggregates and the table's permissions, a `TABLE(...)`
  function type, a relationship and an `EXISTS` predicate on a function without arguments,
  quoted names, named parameters and rejections. `crates/rusuna-http-tests/tests/functions.rs`
  repeats them over `POST` and as a REST endpoint; `subscriptions.rs` multiplexes and subscribes
  to function roots.
- `tests/limits.rs` compares 17 queries of the roles `limited` and `zero`
  (`tests/fixtures/limits.graphql`, `tests/fixtures/limits/`, with the Hasura metadata) with
  the Hasura v2.51.6 statements and their SQL snapshots, and three mutations with Hasura's
  responses inside a rolled-back transaction. The HTTP tests repeat the queries over `POST`, as
  multiplexed statements and as a live subscription.
- `crates/rusuna-http-tests/tests/remote.rs` serves a remote GraphQL server (axum and an
  async-graphql dynamic schema) inside the test and adds it to the role schemas.
- `crates/rusuna-http-tests/tests/rest.rs` builds REST endpoints over the mutation
  fixtures and compares their bodies with the `data` of the GraphQL route.
- `crates/rusuna-http-tests/tests/views.rs` reads a view and a materialized view
  (`tests/fixtures/views.sql`, `views.graphql`) with relationships, permissions and
  aggregates, and checks that read-only types have no mutations.
- `crates/rusuna-http-tests/tests/subscriptions.rs` subscribes over WebSocket with
  `tokio-tungstenite`, waits on the counters of `Subscriptions::stats()` instead of on
  time, and compares them with the `calls` of `pg_stat_statements` in the test database.
  It also runs the multiplexed statement of every admin and permission fixture.
- `crates/rusuna-sqlx/tests/` binds each supported parameter type, runs the multiplexed
  statement for each type and checks mutation transactions on a table of its own. The
  unit tests of `src/subscription.rs` check the placeholder rewrite, including names that
  contain `$1`.
- `RUSUNA_POSTGRES` is a required input of the database tests and the benchmark;
  `tools/with-postgres.sh` sets it and `DATABASE_URL`, which the example reads.
- `crates/rusuna-events/tests/events.rs` creates a database per test. Tests that depend
  on time inject a `Clock` and wait on `Worker::monitor()` poll counters instead of
  sleeping; the retry test asserts only a lower bound on the interval. A webhook test
  serves axum on `127.0.0.1:0`. `crates/rusuna-cli/tests/fixtures/events.sql` is the
  expected migration of `events.json`; rewrite it with `RUSUNA_UPDATE_EXPECTED=1`.
- `cargo package --workspace --exclude rusuna-cli --exclude rusuna-http-tests` packages
  the published crates. A build of the `rusuna` package needs only `.leanfran/cache/`.

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
