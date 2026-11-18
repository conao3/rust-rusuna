# Mutation fixtures

Each JSON case contains a GraphQL document, the role and scalar session, the unmodified
Hasura v2.51.6 HTTP response body, and both tables after execution. `metadata.json`
contains the tracking, relationships and permissions for a source named `default`.
The fixture role's `user_id` becomes the `X-Hasura-User-Id` HTTP header. Admin requests
omit role headers.

To capture a case on Hasura v2.51.6:

1. Create a PostgreSQL source named `default` and execute `schema.sql` on it.
2. Submit `metadata.json` to `/v1/metadata`.
3. Execute `seed.sql` before each case. It resets the identity sequence as well as rows.
4. Submit `{"query": <case.query>}` to `/v1/graphql` with that case's role and session.
5. Capture the response body and the result of `state.sql`.

`tests/mutations.rs` runs the compiled Rust statements against a temporary PostgreSQL
cluster and compares both the response and state. Unordered returning lists are compared as multisets;
explicit relationship ordering and root statement order are retained. A multi-root mutation uses one transaction. Failing
single statements are additionally tested in autocommit mode, and every case executes
under both custom and generic prepared plans. Generated statements are in
`../expected/mutation_<case>.sql`; rejected inputs have no SQL snapshot.

The checks include read-filter-independent top-level returning rows, column restrictions,
relationships and aggregate cycles that read the modified table, different insert and
update checks during upsert, empty writes, database defaults, identity values, write-only
roles and constant-false checks with both empty and nonempty target sets.
