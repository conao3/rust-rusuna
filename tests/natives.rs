use postgres::types::private::BytesMut;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls};
use rusuna::{Error, Schema, Selection, Session, Value};
use serde_json::{Value as Json, json};
use std::error::Error as StdError;

#[derive(Debug)]
struct Param<'a>(&'a Value);

impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn StdError + Sync + Send>> {
        match (self.0, ty) {
            (Value::Int(v), &Type::INT4) => i32::try_from(*v)?.to_sql(ty, out),
            (Value::Int(v), &Type::INT8) => v.to_sql(ty, out),
            (Value::Text(v), &Type::TEXT | &Type::VARCHAR) => v.to_sql(ty, out),
            (Value::Bool(v), &Type::BOOL) => v.to_sql(ty, out),
            (Value::Float(v), &Type::FLOAT8) => v.to_sql(ty, out),
            (Value::Null, _) => Ok(IsNull::Yes),
            (value, ty) => Err(format!("{value:?} is not a {ty}").into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

fn schema() -> Schema {
    Schema::from_sdl(include_str!("fixtures/natives.graphql")).unwrap()
}

fn client() -> Client {
    Client::connect(
        &std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check"),
        NoTls,
    )
    .unwrap()
}

fn run(client: &mut Client, query: &str, session: &Session) -> Json {
    let compiled = schema()
        .compile(query, &json!({}), None, session)
        .unwrap()
        .remove(0)
        .compiled;
    let params: Vec<_> = compiled.params.iter().map(Param).collect();
    let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
    client.query_one(&compiled.sql, &params).unwrap().get(0)
}

fn plain(client: &mut Client, sql: &str) -> Json {
    client.query_one(sql, &[]).unwrap().get(0)
}

const SEARCH: &str = "(SELECT a.id, a.author_id, a.title, length(a.title) AS size FROM article AS a WHERE (a.id >= 3 AND a.title LIKE 'title 1%') OR a.id = 300)";

#[test]
fn a_statement_root_reads_like_a_table() {
    let mut client = client();
    let admin = Session::new("admin");
    let actual = run(
        &mut client,
        "{ article_search(min_id: 3, pattern: \"title 1%\", where: {id: {_lt: 12}}, order_by: {id: desc}, limit: 2, offset: 0) { id title size author { name } comments(order_by: {id: asc}) { id } comments_aggregate { aggregate { count } } stats { articles } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        &format!(
            "SELECT json_agg(json_build_object('id', s.id, 'title', s.title, 'size', s.size, 'author', json_build_object('name', au.name), 'comments', (SELECT coalesce(json_agg(json_build_object('id', c.id) ORDER BY c.id), '[]') FROM comment c WHERE c.article_id = s.id), 'comments_aggregate', json_build_object('aggregate', json_build_object('count', (SELECT count(*) FROM comment c WHERE c.article_id = s.id))), 'stats', (SELECT json_build_object('articles', count(*)) FROM article x WHERE x.author_id = s.author_id)) ORDER BY s.id DESC) FROM (SELECT * FROM {SEARCH} AS s WHERE s.id < 12 ORDER BY s.id DESC LIMIT 2) AS s JOIN author au ON au.id = s.author_id"
        ),
    );
    assert_eq!(actual, expected);
    assert_eq!(actual.as_array().unwrap().len(), 2);
    let actual = run(
        &mut client,
        "{ article_search_aggregate(min_id: 3, pattern: \"title 1%\") { aggregate { count sum { size } max { id } } nodes { id } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        &format!(
            "SELECT json_build_object('aggregate', json_build_object('count', count(*), 'sum', json_build_object('size', sum(s.size)), 'max', json_build_object('id', max(s.id))), 'nodes', json_agg(json_build_object('id', s.id))) FROM {SEARCH} AS s"
        ),
    );
    let sorted = |mut value: Json| {
        value["nodes"]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|node| node["id"].as_i64());
        value
    };
    assert_eq!(sorted(actual), sorted(expected));
}

#[test]
fn tables_read_statements_without_arguments_through_relationships_and_filters() {
    let mut client = client();
    let admin = Session::new("admin");
    let actual = run(
        &mut client,
        "{ author(where: {stats: {articles: {_gte: 3}}}, order_by: {id: asc}) { id stats { articles latest author { name } } stats_aggregate { aggregate { sum { articles } } } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('id', a.id, 'stats', (SELECT coalesce(json_agg(json_build_object('articles', s.articles, 'latest', s.latest, 'author', json_build_object('name', a.name))), '[]') FROM (SELECT author_id, count(*)::int AS articles, max(published_at) AS latest FROM article GROUP BY author_id) s WHERE s.author_id = a.id), 'stats_aggregate', json_build_object('aggregate', json_build_object('sum', json_build_object('articles', (SELECT sum(s.articles) FROM (SELECT author_id, count(*)::int AS articles FROM article GROUP BY author_id) s WHERE s.author_id = a.id))))) ORDER BY a.id) FROM author a WHERE (SELECT count(*) FROM article x WHERE x.author_id = a.id) >= 3",
    );
    assert_eq!(actual, expected);
    assert_eq!(actual.as_array().unwrap().len(), 2);
}

#[test]
fn arguments_are_parameters_in_the_order_of_their_markers() {
    let schema = schema();
    let pattern = "x'); DROP TABLE article; --";
    let compiled = schema
        .compile(
            "query Q($pattern: String) { article_search(min_id: 7, pattern: $pattern, where: {size: {_gt: 1}}) { id } }",
            &json!({ "pattern": pattern }),
            None,
            &Session::new("admin"),
        )
        .unwrap()
        .remove(0)
        .compiled;
    assert_eq!(
        compiled.params,
        vec![
            Value::Int(7),
            Value::Text(pattern.into()),
            Value::Int(7),
            Value::Int(1)
        ]
    );
    assert!(!compiled.sql.contains(pattern));
    assert!(!compiled.sql.contains(" 7 "));
    let other = schema
        .compile(
            "{ article_search(min_id: 8, pattern: \"y\", where: {size: {_gt: 2}}) { id } }",
            &json!({}),
            None,
            &Session::new("admin"),
        )
        .unwrap()
        .remove(0)
        .compiled;
    assert_eq!(other.sql, compiled.sql);
    let omitted = schema
        .compile(
            "{ article_search(min_id: 7) { id } }",
            &json!({}),
            None,
            &Session::new("admin"),
        )
        .unwrap()
        .remove(0)
        .compiled;
    assert_eq!(
        omitted.params,
        vec![Value::Int(7), Value::Null, Value::Int(7)]
    );
    assert!(matches!(
        schema.compile(
            "{ article_search { id } }",
            &json!({}),
            None,
            &Session::new("admin")
        ),
        Err(Error::Document { .. })
    ));
}

#[test]
fn permissions_restrict_statement_rows_and_columns() {
    let mut client = client();
    let user = Session::new("user").variable("user_id", Value::Int(3));
    let actual = run(
        &mut client,
        "{ article_search(min_id: 1, pattern: \"title%\", order_by: {id: asc}) { id title size } }",
        &user,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('id', id, 'title', title, 'size', length(title)) ORDER BY id) FROM article WHERE author_id = 3",
    );
    assert_eq!(actual, expected);
    let actual = run(
        &mut client,
        "{ author_stats(order_by: {author_id: asc}) { author_id articles } }",
        &user,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('author_id', author_id, 'articles', n) ORDER BY author_id) FROM (SELECT author_id, count(*)::int AS n FROM article GROUP BY author_id) s WHERE n >= 2",
    );
    assert_eq!(actual, expected);
    let schema = schema();
    for query in [
        "{ article_search(min_id: 1) { author_id } }",
        "{ author_stats { latest } }",
        "{ typed(i: 1, f: 1.0, s: \"a\", b: true, d: \"2020-01-01T00:00:00Z\") { i } }",
    ] {
        assert!(
            schema.compile(query, &json!({}), None, &user).is_err(),
            "{query}"
        );
    }
    let sdl = schema.expanded_sdl("user").unwrap();
    assert!(
        sdl.contains(
            "article_search(min_id: Int!, pattern: String, where: article_search_bool_exp"
        )
    );
    assert!(!sdl.contains("typed"));
    assert!(
        !schema
            .expanded_sdl("nobody")
            .unwrap()
            .contains("article_search")
    );
}

#[test]
fn statements_have_no_mutations_and_their_declarations_are_checked() {
    let schema = schema();
    let sdl = schema.expanded_sdl("admin").unwrap();
    assert!(!sdl.contains("insert_article_search"));
    assert!(!sdl.contains("article_search_by_pk"));
    let base = include_str!("fixtures/natives.graphql");
    for (from, to) in [
        (
            "stats: author_stats @relation",
            "search: [article_search!]! @relation(columns: [\"id\"], references: [\"author_id\"])\n  stats: author_stats @relation",
        ),
        (
            "type author_stats @native_query(",
            "type author_stats @primary_key(columns: [\"author_id\"]) @native_query(",
        ),
        (
            "type author_stats @native_query(",
            "type author_stats @table(name: \"author\") @native_query(",
        ),
        ("{{i}}::int", "{{j}}::int"),
        ("{{i}}::int", "$1::int"),
        ("{{i}}::int", "{{i}}::int; SELECT 1"),
        ("min_id: \"Int!\"", "min_id: \"Unknown\""),
        ("min_id: \"Int!\"", "where: \"Int\""),
        (
            "type author_stats @native_query(",
            "type author_stats @insert_permission(role: \"user\", columns: [\"articles\"], check: {}) @native_query(",
        ),
    ] {
        let sdl = base.replacen(from, to, 1);
        assert_ne!(sdl, base, "{from}");
        assert!(Schema::from_sdl(&sdl).is_err(), "{to}");
    }
}

#[test]
fn lower_level_selections_need_the_declared_arguments_at_the_root_only() {
    let schema = schema();
    let admin = Session::new("admin");
    let id = || Selection::new("id");
    for selection in [
        Selection::new("article_search").field(id()),
        Selection::new("article_search")
            .argument("pattern", Value::Null)
            .argument("min_id", Value::Int(1))
            .field(id()),
        Selection::new("article_search")
            .argument("min_id", Value::Float(f64::NAN))
            .argument("pattern", Value::Null)
            .field(id()),
        Selection::new("author")
            .argument("min_id", Value::Int(1))
            .field(id()),
        Selection::new("author").field(
            Selection::new("stats")
                .argument("x", Value::Int(1))
                .field(Selection::new("articles")),
        ),
    ] {
        assert!(
            matches!(
                schema.compile_selection(&selection, &admin),
                Err(Error::InvalidArguments { .. })
            ),
            "{selection:?}"
        );
    }
    let compiled = schema
        .compile_selection(
            &Selection::new("article_search")
                .argument("min_id", Value::Int(1))
                .argument("pattern", Value::Null)
                .field(id()),
            &admin,
        )
        .unwrap();
    assert_eq!(
        compiled.params,
        vec![Value::Int(1), Value::Null, Value::Int(1)]
    );
}

#[test]
fn lower_level_statements_are_checked_like_sdl_statements() {
    use rusuna::{Body, RootField, RootKind, SqlPart, Table};
    let build = |parts: Vec<SqlPart>| {
        let mut table = Table::new("t", "", "").read_only().column("id", "id");
        table.body = Body::Statement {
            parts,
            arguments: vec!["a".into()],
        };
        Schema::new(vec![table], vec![RootField::new("t", "t", RootKind::List)])
    };
    let text = |s: &str| SqlPart::Text(s.into());
    let argument = || SqlPart::Argument("a".into());
    assert!(build(vec![text("SELECT "), argument(), text(" AS id")]).is_ok());
    for parts in [
        vec![text("SELECT $1 AS id")],
        vec![text("SELECT 1 AS id; SELECT "), argument()],
        vec![text("SELECT 1 AS id -- "), argument()],
        vec![text("SELECT '{{a}}' AS id")],
        vec![text("SELECT "), SqlPart::Argument("b".into())],
    ] {
        assert!(
            matches!(build(parts.clone()), Err(Error::InvalidStatement { .. })),
            "{parts:?}"
        );
    }
}
