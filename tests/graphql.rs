mod common;

use rusuna::{Error, Schema};
use serde_json::json;

#[test]
fn every_fixture_parses_to_the_existing_selection() {
    let schema = common::schema();
    let cases = common::cases();
    assert_eq!(cases.len(), 43);
    for case in cases {
        let query = std::fs::read_to_string(format!(
            "{}/tests/fixtures/queries/{}.graphql",
            env!("CARGO_MANIFEST_DIR"),
            case.name
        ))
        .unwrap();
        assert_eq!(
            schema.parse_query(&query, &json!({}), None).unwrap(),
            vec![case.query],
            "{}",
            case.name
        );
    }
}

#[test]
fn expanded_schema_matches_the_fixture_and_parses() {
    let schema = common::schema();
    let expanded = schema.expanded_sdl().unwrap();
    assert_eq!(expanded, include_str!("fixtures/schema.expanded.graphql"));
    async_graphql_parser::parse_schema(expanded).unwrap();
    assert_eq!(expanded, common::schema().expanded_sdl().unwrap());
}

#[test]
fn variables_defaults_fragments_and_directives_compile_to_the_same_statements() {
    let schema = common::schema();
    let expected = schema
        .compile(include_str!("fixtures/queries/a.graphql"), &json!({}), None)
        .unwrap();
    let query = r#"
        query A($limit: Int = 20, $show: Boolean! = true, $skip: Boolean! = false, $order: [article_order_by!] = {published_at: desc}) {
          article(limit: $limit, order_by: $order) {
            ...ArticleFields @include(if: $show)
            comments(limit: 5, order_by: {id: asc}) @skip(if: $skip) { ... on comment { body } }
            ignored: id @skip(if: true)
          }
        }
        fragment ArticleFields on article { id title author { name } }
        query Other { author { id } }
    "#;
    assert_eq!(
        expected,
        schema.compile(query, &json!({}), Some("A")).unwrap()
    );
    assert_eq!(
        expected,
        schema
            .compile(
                query,
                &json!({"limit":20,"show":true,"skip":false,"order":{"published_at":"desc"}}),
                Some("A")
            )
            .unwrap()
    );
    let nested = "query($id: Int = 3) { article(where: {author_id: {_eq: $id}}, order_by: {id: desc}) { id title comments(where: {article_id: {_eq: 9}}) { id body } author { id name articles(limit: 2, order_by: {id: asc}) { id } } } }";
    assert_eq!(
        schema.compile(nested, &json!({}), None).unwrap(),
        schema
            .compile(include_str!("fixtures/queries/d.graphql"), &json!({}), None)
            .unwrap()
    );
}

#[test]
fn undefined_optional_variables_are_omitted_and_null_overrides_defaults() {
    let schema = common::schema();
    let query = "query($limit: Int) { article(limit: $limit) { id } }";
    assert_eq!(
        schema.compile(query, &json!({}), None).unwrap(),
        schema
            .compile("{ article { id } }", &json!({}), None)
            .unwrap()
    );
    let query = "query($limit: Int = 1) { article(limit: $limit) { id } }";
    assert_eq!(
        schema.compile(query, &json!({"limit":null}), None).unwrap(),
        schema
            .compile("{ article { id } }", &json!({}), None)
            .unwrap()
    );
}

#[test]
fn aliases_multiple_roots_and_repeated_fragment_fields_are_collected() {
    let schema = common::schema();
    let query =
        "{ first: author { id ...F } second: article { title } } fragment F on author { id name }";
    let actual = schema.compile(query, &json!({}), None).unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|v| v.response_key.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(
        actual[0].compiled,
        schema
            .compile("{ author { id name } }", &json!({}), None)
            .unwrap()[0]
            .compiled
    );
    assert!(
        schema
            .compile("{ author { x: id x: name } }", &json!({}), None)
            .is_err()
    );
    assert!(
        schema
            .compile(
                "{ a: author(limit: 1) { id } a: author(limit: 2) { id } }",
                &json!({}),
                None
            )
            .is_err()
    );
    assert!(
        schema
            .compile("{ author @skip(if: true) { id } }", &json!({}), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn typed_inputs_and_unsupported_operations_are_rejected() {
    let schema = common::schema();
    for query in [
        "{ author(where: {id: {_eq: \"1\"}}) { id } }",
        "{ author(where: {id: {_like: \"1%\"}}) { id } }",
        "{ author(where: {rating: {_eq: true}}) { id } }",
        "{ author(where: {id: {_in: [null]}}) { id } }",
        "{ author(where: {id: {_eq: null}}) { id } }",
        "{ author(where: {id: {_is_null: 1}}) { id } }",
        "{ author(where: {id: {_eq: 2147483648}}) { id } }",
        "{ author(where: {id: {_eq: 1, _eq: 2}}) { id } }",
        "{ author(where: {id: {_eq: 1}, id: {_eq: 2}}) { id } }",
        "{ author_aggregate { aggregate { sum { name } } } }",
        "{ author_aggregate { aggregate { count(columns: [id]) } } }",
        "{ article { author(limit: 1) { id } } }",
        "{ author(order_by: {id: \"asc\"}) { id } }",
        "{ author(order_by: {id: sideways}) { id } }",
        "{ author(limit: -1) { id } }",
        "{ author(limit: 1, limit: 2) { id } }",
        "{ author(where: {missing: {_eq: 1}}) { id } }",
        "{ author { unknown @skip(if: true) } }",
        "{ author { id @unknown } }",
        "{ author { id @skip(if: 1) } }",
        "{ author { id @skip(if: true) @skip(if: false) } }",
        "{ author { ... on article { id } } }",
        "{ author { ...Missing } }",
        "{ author { ...A } } fragment A on author { ...B } fragment B on author { ...A }",
        "query($v: String = \"2\") { author(limit: $v) { id } }",
        "query($v: Int) { author { id @include(if: $v) } }",
        "query($v: Boolean) { author { id @include(if: $v) } }",
        "query($v: Int!) { author(limit: $v) { id } }",
        "{ author(limit: $v) { id } }",
        "mutation { author { id } }",
        "subscription { author { id } }",
        "{ __schema { queryType { name } } }",
        "{ __type(name: \"author\") { name } }",
        "query A { author { id } } query B { article { id } }",
    ] {
        assert!(schema.compile(query, &json!({}), None).is_err(), "{query}");
    }
    assert!(
        schema
            .compile(
                "query($v: Int) { author(limit: $v) { id } }",
                &json!({"v":"1"}),
                None
            )
            .is_err()
    );
    assert!(
        schema
            .compile("{ author { id } }", &json!([]), None)
            .is_err()
    );
}

#[test]
fn invalid_sdl_reports_source_positions() {
    for sdl in [
        "type Query { rows: [Missing!]! @list }",
        "type A @table(name: \"a\") { id: Int! } type Query { rows: [A!]! @list @list }",
        "type A @table(name: \"a\", schema: 1) { id: Int! } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { id: Int! child: A @relation(columns: [\"id\"], references: []) } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { id: Int! @column(name: \"id\", name: \"other\") } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { id: Missing } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { id: Int! id: Int! } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { _and: Int! } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { _or: Int! } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { _not: Int! } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { id: Int! } type Query { rows: A @list }",
        "type A @table(name: \"a\") { id: Int! } type Query { rows: A_aggregate @aggregate(target: \"Missing\") }",
        "type A @table(name: \"a\") { id: Int! } type Query { rows: A_aggregate @aggregate(target: \"A\") @list }",
        "type A @table(name: \"a\") { id: Int! @typo } type Query { rows: [A!]! @list }",
        "type A @table(name: \"a\") { id: Int! } type Query { rows: [A!]! @list } type A_order_by @table(name: \"b\") { id: Int! }",
    ] {
        let Error::Document { line, column, .. } = Schema::from_sdl(sdl).unwrap_err() else {
            panic!("missing position: {sdl}");
        };
        assert!(line > 0 && column > 0);
    }
    let error = Schema::from_sdl("type Query {\n  rows: [Missing!]! @list\n}").unwrap_err();
    assert!(matches!(error, Error::Document { line: 2, .. }));
}

#[test]
fn input_and_fragment_recursion_is_bounded_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let schema = common::schema();
            let query = format!(
                "{{ author(where: {}{{}}{}) {{ id }} }}",
                "{_not:".repeat(1000),
                "}".repeat(1000)
            );
            assert!(schema.compile(&query, &json!({}), None).is_err());
            let mut query = "{ author { ...F0 } }".to_string();
            for n in 0..150 {
                query.push_str(&format!(" fragment F{n} on author {{ ...F{} }}", n + 1));
            }
            query.push_str(" fragment F150 on author { id }");
            assert!(schema.compile(&query, &json!({}), None).is_err());
            let query = format!(
                "{{ author(where: {{name: {{_eq: \"{}\"}}}}) {{ id }} }}",
                "[".repeat(500)
            );
            assert!(schema.compile(&query, &json!({}), None).is_ok());
            let query = format!(
                r#"{{ author(where: {{name: {{_eq: """a\\"""{}"""}}}}) {{ id }} }}"#,
                "[".repeat(500)
            );
            assert!(schema.compile(&query, &json!({}), None).is_ok());
        })
        .unwrap()
        .join()
        .unwrap();
}
