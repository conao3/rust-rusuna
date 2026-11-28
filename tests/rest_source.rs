use rusuna::{Error, Schema, Selection, Session};
use serde_json::json;

const SDL: &str = r#"
    scalar JSON
    enum Visibility { public private }
    input Options { filter: Visibility = public }
    type Query {
      rows: [Row!]! @list
      feed: Feed @rest(source: "api", path: "/feed", roles: ["user"])
      secret(opts: Options = {}): Hidden @rest(source: "api", path: "/secret")
    }
    type Row @table(name: "rows") @permission(role: "user", columns: ["id"], where: {}) { id: Int! }
    type Feed {
      id: Int @column(name: "@id")
      public: JSON
      secret: Hidden @rest(source: "api", path: "/secret/{{id}}")
    }
    type Hidden { flag: Visibility }
"#;

#[test]
fn rest_fields_and_reachable_types_are_expanded_for_the_role() {
    let schema = Schema::from_sdl(SDL).unwrap();
    assert_eq!(schema.roles(), ["admin", "user"]);
    let admin = schema.expanded_sdl("admin").unwrap();
    let user = schema.expanded_sdl("user").unwrap();
    let guest = schema.expanded_sdl("guest").unwrap();
    for sdl in [&admin, &user, &guest] {
        async_graphql_parser::parse_schema(sdl).unwrap();
    }
    assert!(admin.contains("secret(opts: Options = {}): Hidden @rest("));
    assert!(admin.contains("filter: Visibility = public"));
    assert!(admin.contains("enum Visibility"));
    assert!(user.contains("feed: Feed @rest("));
    assert!(user.contains("id: Int @column(name: \"@id\")"));
    assert!(user.contains("scalar JSON"));
    assert!(user.contains("type Subscription {\n  rows("));
    for hidden in ["type Hidden", "enum Visibility", "input Options", "secret:"] {
        assert!(!user.contains(hidden), "{hidden}: {user}");
    }
    for hidden in ["type Feed", "type Hidden", "scalar JSON", "@rest("] {
        assert!(!guest.contains(hidden), "{hidden}: {guest}");
    }
    let subscription = admin
        .split("type Subscription")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(subscription.contains("rows("));
    assert!(!subscription.contains("@rest"));
}

#[test]
fn compiling_a_document_with_a_rest_root_returns_rest_field() {
    let schema = Schema::from_sdl(SDL).unwrap();
    let session = Session::new("user");
    let variables = json!({});
    let document = "query SQL { rows { id } } query Rest { rows { id } ...Remote } fragment Remote on Query { renamed: feed { id } }";
    let error = Error::RestField {
        field: "feed".into(),
    };
    assert_eq!(
        schema.compile(document, &variables, Some("Rest"), &session),
        Err(error.clone())
    );
    assert_eq!(
        schema.compile_selection(&Selection::new("feed"), &session),
        Err(error)
    );
    assert!(
        schema
            .compile(document, &variables, Some("SQL"), &session)
            .is_ok()
    );
    assert!(
        matches!(schema.compile("query($options: Options) { secret(opts: $options) { flag } }", &variables, None, &session), Err(Error::RestField { field }) if field == "secret")
    );
}

#[test]
fn rest_mutation_roots_follow_roles_and_report_sql_compilation_errors() {
    let schema = Schema::from_sdl(&format!(
        "{SDL}{}",
        r#"
        type Mutation {
          publish(flag: Boolean): Published
            @rest(source: "api", method: PATCH, path: "/publish", body: {flag: "{{flag}}"}, roles: ["user"])
          remove: Boolean @rest(source: "api", method: DELETE, path: "/remove")
        }
        type Published { time: DateTime }
        "#
    ))
    .unwrap();
    let user = schema.expanded_sdl("user").unwrap();
    assert!(user.contains("publish(flag: Boolean): Published @rest("));
    assert!(user.contains("scalar DateTime"));
    assert!(!user.contains("remove:"));
    let guest = schema.expanded_sdl("guest").unwrap();
    assert!(!guest.contains("publish("));
    assert!(!guest.contains("Published"));
    assert_eq!(
        schema.compile(
            "mutation { ...Remote } fragment Remote on Mutation { publish { time } }",
            &json!({}),
            None,
            &Session::new("user"),
        ),
        Err(Error::RestField {
            field: "publish".into()
        })
    );
}

#[test]
fn the_loader_rejects_invalid_rest_definitions() {
    for schema in [
        r#"type Query { info: String @rest(source: "api", path: "https://other.example/a") }"#,
        r#"type Query { info: String @rest(source: "api", path: "//other.example/a") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a/../b") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a/%2e%2e/b") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a%2Fb") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a?b") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a#b") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/{{unknown}}") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/{{broken}") }"#,
        r#"type Query { info: String @rest(source: "api", method: POST, path: "/a") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a", body: {}) }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Mutation { write: String @rest(source: "api", path: "/b") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Mutation { write: String @rest(source: "api", method: GET, path: "/b") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Mutation { write: String @rest(source: "api", method: POST, path: "/b", body: []) }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Mutation { write: String @rest(source: "api", method: POST, path: "/b", body: {nested: ["{{unknown}}"]}) }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Mutation { write: String @rest(source: "api", method: POST, path: "/b") @column(name: "other") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Mutation { write: String }"#,
        r#"type Query { rows: [Row!]! @list } type Row @table(name: "rows") { id: Int } type Mutation { insert_Row: String @rest(source: "api", method: POST, path: "/b") }"#,
        r#"type Query { info: String! @rest(source: "api", path: "/a", not_found: null) }"#,
        r#"type Query { info(n: Int = "two"): String @rest(source: "api", path: "/a") }"#,
        r#"type Query { info: Row @rest(source: "api", path: "/a") } type Row @table(name: "rows") { id: Int }"#,
        r#"type Query { rows(id: Int): [Row!]! @list } type Row @table(name: "rows") { id: Int }"#,
        r#"type Query { rows: [Row!]! @list } type Row @table(name: "rows") { id: Int, ext: String @rest(source: "api", path: "/a") }"#,
        r#"type Query { info: Info @rest(source: "api", path: "/a") } type Info @permission(role: "user", where: {}, columns: ["id"]) { id: Int }"#,
        r#"type Query { info: Info @rest(source: "api", path: "/a") } type Info { ids: [Int], next: String @rest(source: "api", path: "/a/{{ids}}") }"#,
        r#"type Query { info: String @rest(source: "api", path: "/a") } type Orphan { id: Int }"#,
    ] {
        assert!(Schema::from_sdl(schema).is_err(), "{schema}");
    }
}
