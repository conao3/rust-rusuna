use rusuna::{Error, Schema, Session, Value};
use serde_json::json;

fn schema() -> Schema {
    Schema::from_sdl(include_str!("fixtures/permissions.graphql")).unwrap()
}

#[test]
fn session_values_are_parameters_and_do_not_change_sql() {
    let schema = schema();
    let query =
        "{ article(where: {id: {_gt: 1}}) { id comments(where: {id: {_gt: 2}}) { body } } }";
    let first = schema
        .compile(
            query,
            &json!({}),
            None,
            &Session::new("user").variable("user_id", Value::Int(3)),
        )
        .unwrap();
    let second = schema
        .compile(
            query,
            &json!({}),
            None,
            &Session::new("user").variable("user_id", Value::Int(1)),
        )
        .unwrap();
    assert_eq!(first[0].compiled.sql, second[0].compiled.sql);
    assert_eq!(
        first[0].compiled.params,
        vec![Value::Int(1), Value::Int(3), Value::Int(2), Value::Int(3)]
    );
    assert_eq!(
        second[0].compiled.params,
        vec![Value::Int(1), Value::Int(1), Value::Int(2), Value::Int(1)]
    );
    assert_eq!(
        schema.compile(query, &json!({}), None, &Session::new("user")),
        Err(Error::MissingSession {
            name: "user_id".into()
        })
    );
}

#[test]
fn inaccessible_tables_columns_and_aggregates_are_unknown_fields() {
    let schema = schema();
    for (role, query) in [
        ("user", "{ revision { note } }"),
        ("user", "{ article { revisions { note } } }"),
        (
            "user",
            "{ article(where: {revisions: {note: {_eq: \"x\"}}}) { id } }",
        ),
        ("user", "{ author { rating } }"),
        ("user", "{ author(where: {rating: {_gt: 0.0}}) { id } }"),
        ("user", "{ author(order_by: {rating: asc}) { id } }"),
        (
            "user",
            "{ author_aggregate { aggregate { sum { rating } } } }",
        ),
        ("reviewer", "{ author_aggregate { aggregate { count } } }"),
        ("reviewer", "{ article { comments { body } } }"),
        ("reviewer", "{ article { author { name } } }"),
        ("reviewer", "{ article { author_id } }"),
        ("unknown", "{ article { id } }"),
    ] {
        let result = schema.compile(
            query,
            &json!({}),
            None,
            &Session::new(role).variable("user_id", Value::Int(3)),
        );
        assert!(
            matches!(result, Err(Error::Document { .. })),
            "{role}: {query}: {result:?}"
        );
    }
    assert!(
        schema
            .compile(
                "{ revision { note } }",
                &json!({}),
                None,
                &Session::new("admin")
            )
            .is_ok()
    );
}

#[test]
fn expanded_sdl_only_exposes_the_roles_fields() {
    let schema = schema();
    let user = schema.expanded_sdl("user").unwrap();
    assert!(!user.contains("revision"));
    assert!(!user.contains("rating"));
    assert!(user.contains("author: author\n"));
    assert!(user.contains("article_aggregate"));
    async_graphql_parser::parse_schema(&user).unwrap();
    let reviewer = schema.expanded_sdl("reviewer").unwrap();
    assert!(!reviewer.contains("comment"));
    assert!(!reviewer.contains("author_aggregate"));
    assert!(!reviewer.contains("author_id"));
    async_graphql_parser::parse_schema(&reviewer).unwrap();
    async_graphql_parser::parse_schema(schema.expanded_sdl("unknown").unwrap()).unwrap();
}

#[test]
fn relationship_permission_dependencies_must_be_explicit() {
    let sdl = include_str!("fixtures/permissions.graphql").replace(
        "where: {article: {author_id: {_eq: \"$user_id\"}}}",
        "where: {article: {id: {_gt: 0}}}",
    );
    let schema = Schema::from_sdl(&sdl).unwrap();
    assert!(matches!(
        schema.compile(
            "{ comment { id } }",
            &json!({}),
            None,
            &Session::new("user").variable("user_id", Value::Int(3))
        ),
        Err(Error::InvalidPermission { .. })
    ));
}

#[test]
fn permission_directives_validate_roles_columns_and_supported_arguments() {
    for directive in [
        "@permission(role: \"admin\", columns: [\"id\"], where: {})",
        "@permission(role: \"user\", columns: [\"missing\"], where: {})",
        "@permission(role: \"user\", columns: [\"id\", \"id\"], where: {})",
        "@permission(role: \"user\", columns: [\"id\"], where: {missing: {_eq: 1}})",
        "@permission(role: \"user\", columns: [\"id\"], where: {id: {_eq: \"bad\"}})",
        "@permission(role: \"user\", columns: [\"id\"], where: {}, limit: 2)",
        "@permission(role: \"user\", columns: [\"id\"], where: {}, allow_aggregations: 1)",
    ] {
        assert!(Schema::from_sdl(&format!("type A @table(name: \"a\") {directive} {{ id: Int! }} type Query {{ rows: [A!]! @list }}")).is_err(), "{directive}");
    }
}

#[test]
fn lower_level_queries_cannot_bypass_column_permissions() {
    use rusuna::{Filter, Selection};
    let schema = schema();
    let session = Session::new("reviewer");
    for query in [
        Selection::new("article").field(Selection::new("author_id")),
        Selection::new("article")
            .filter(Filter::eq("author_id", Value::Int(3)))
            .field(Selection::new("id")),
        Selection::new("comment").field(Selection::new("id")),
    ] {
        assert!(schema.compile_selection(&query, &session).is_err());
    }
}

#[test]
fn session_types_and_literal_dollar_strings_are_checked() {
    let schema = schema();
    for value in [
        Value::Null,
        Value::Text("3".into()),
        Value::Bool(true),
        Value::Float(f64::NAN),
    ] {
        assert!(
            schema
                .compile(
                    "{ article { id } }",
                    &json!({}),
                    None,
                    &Session::new("user").variable("user_id", value)
                )
                .is_err()
        );
    }
    let schema = Schema::from_sdl(r#"type A @table(name: "a") @permission(role: "user", columns: ["text"], where: {_or: [{text: {_eq: "$$price"}}, {text: {_eq: "$text"}}]}) { text: String! } type Query { rows: [A!]! @list }"#).unwrap();
    let malicious = "'); DROP TABLE a; --";
    let compiled = schema
        .compile(
            "{ rows { text } }",
            &json!({}),
            None,
            &Session::new("user").variable("text", Value::Text(malicious.into())),
        )
        .unwrap();
    assert_eq!(
        compiled[0].compiled.params,
        vec![Value::Text("$price".into()), Value::Text(malicious.into())]
    );
    assert!(!compiled[0].compiled.sql.contains(malicious));
}

#[test]
fn maximum_permission_filters_compile_on_a_two_mib_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            use rusuna::{Filter, Operand, Permission, RootField, RootKind, Selection, Table};
            let build = |filter| {
                let mut table = Table::new("A", "public", "a").column("id", "id").relation(
                    rusuna::Relation::new("related", rusuna::Kind::Array, "A", ["id"], ["id"]),
                );
                table.permissions.push(Permission {
                    role: "user".into(),
                    filter,
                    columns: vec!["id".into()],
                    allow_aggregations: true,
                });
                Schema::new(
                    vec![table],
                    vec![RootField::new("rows", "A", RootKind::List)],
                )
            };
            let leaf = Filter::In {
                field: "id".into(),
                negated: false,
                values: vec![Operand::Session("id".into()); rusuna::MAX_IN_VALUES],
            };
            let wide = Filter::And(vec![
                Filter::eq("id", Operand::Literal(Value::Int(1)));
                rusuna::MAX_FIELDS
            ]);
            for (filter, depth) in [
                (leaf, rusuna::MAX_FILTER_DEPTH),
                (wide, rusuna::MAX_FILTER_DEPTH - 1),
            ] {
                let filter = (0..depth).fold(filter, |f, _| Filter::Not(Box::new(f)));
                let schema = build(filter).unwrap();
                let compiled = schema
                    .compile_selection(
                        &Selection::new("rows").field(Selection::new("id")),
                        &Session::new("user").variable("id", Value::Int(1)),
                    )
                    .unwrap();
                assert_eq!(compiled.params.len(), 1000);
            }
            let policy = Filter::And(vec![
                Filter::eq("id", Operand::Literal(Value::Int(1)));
                rusuna::MAX_FIELDS
            ]);
            let policy =
                (0..rusuna::MAX_FILTER_DEPTH - 1).fold(policy, |f, _| Filter::Not(Box::new(f)));
            let schema = build(policy).unwrap();
            let filter =
                (0..rusuna::MAX_FILTER_DEPTH).fold(Filter::eq("id", Value::Int(2)), |f, _| {
                    Filter::Relation {
                        field: "related".into(),
                        filter: Box::new(f),
                    }
                });
            let inner = Selection::new("related")
                .filter(filter)
                .field(Selection::new("id"));
            let inner = (1..rusuna::MAX_DEPTH)
                .fold(inner, |inner, _| Selection::new("related").field(inner));
            let compiled = schema
                .compile_selection(&Selection::new("rows").field(inner), &Session::new("user"))
                .unwrap();
            assert_eq!(
                compiled.params.len(),
                (rusuna::MAX_DEPTH + rusuna::MAX_FILTER_DEPTH + 1) * rusuna::MAX_FIELDS + 1
            );
            let deep = (0..=rusuna::MAX_FILTER_DEPTH)
                .fold(Filter::And(vec![]), |f, _| Filter::Not(Box::new(f)));
            assert!(build(deep).is_err());
            let wide = build(Filter::And(vec![
                Filter::eq(
                    "id",
                    Operand::Literal(Value::Int(1))
                );
                rusuna::MAX_FIELDS + 1
            ]))
            .unwrap();
            assert!(
                wide.compile_selection(
                    &Selection::new("rows").field(Selection::new("id")),
                    &Session::new("user")
                )
                .is_err()
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
