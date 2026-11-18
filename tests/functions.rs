use postgres::types::private::BytesMut;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls};
use rusuna::{
    Body, Error, Kind, Relation, RootField, RootKind, Schema, Selection, Session, Table, Value,
};
use serde_json::{Value as Json, json};
use std::error::Error as StdError;
use std::sync::OnceLock;

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
            (Value::Text(v), &Type::TEXT | &Type::VARCHAR) => v.to_sql(ty, out),
            (Value::Null, _) => Ok(IsNull::Yes),
            (value, ty) => Err(format!("{value:?} is not a {ty}").into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

fn client() -> Client {
    static FUNCTIONS: OnceLock<()> = OnceLock::new();
    let config = std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check");
    FUNCTIONS.get_or_init(|| {
        Client::connect(&config, NoTls)
            .unwrap()
            .batch_execute(include_str!("fixtures/functions.sql"))
            .unwrap();
    });
    Client::connect(&config, NoTls).unwrap()
}

fn schema() -> Schema {
    Schema::from_sdl(include_str!("fixtures/functions.graphql")).unwrap()
}

fn run(client: &mut Client, query: &str, session: &Session) -> Json {
    let compiled = schema()
        .compile(query, &json!({}), None, session)
        .unwrap()
        .remove(0)
        .compiled;
    fetch(client, &compiled)
}

fn fetch(client: &mut Client, compiled: &rusuna::Compiled) -> Json {
    let params: Vec<_> = compiled.params.iter().map(Param).collect();
    let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
    client.query_one(&compiled.sql, &params).unwrap().get(0)
}

fn plain(client: &mut Client, sql: &str) -> Json {
    client.query_one(sql, &[]).unwrap().get(0)
}

#[test]
fn a_function_returning_table_rows_reads_like_the_table() {
    let mut client = client();
    let admin = Session::new("admin");
    let actual = run(
        &mut client,
        "{ search_articles(search: \"title 1%\", since_id: 10, where: {author_id: {_gt: 1}}, order_by: {id: desc}, limit: 5) { id title author { name latest { title } } comments(order_by: {id: asc}) { id } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('id', s.id, 'title', s.title, 'author', json_build_object('name', au.name, 'latest', (SELECT json_build_object('title', l.title) FROM latest_titles() l WHERE l.author_id = au.id)), 'comments', (SELECT coalesce(json_agg(json_build_object('id', c.id) ORDER BY c.id), '[]') FROM comment c WHERE c.article_id = s.id)) ORDER BY s.id DESC) FROM (SELECT * FROM search_articles('title 1%', 10) WHERE author_id > 1 ORDER BY id DESC LIMIT 5) s JOIN author au ON au.id = s.author_id",
    );
    assert_eq!(actual, expected);
    assert_eq!(actual.as_array().unwrap().len(), 2);
    let actual = run(
        &mut client,
        "{ search_articles_aggregate(search: \"title%\") { aggregate { count max { id } } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        "SELECT json_build_object('aggregate', json_build_object('count', count(*), 'max', json_build_object('id', max(id)))) FROM search_articles('title%', NULL)",
    );
    assert_eq!(actual, expected);
}

#[test]
fn a_function_returning_a_table_type_is_a_root_with_relationships() {
    let mut client = client();
    let admin = Session::new("admin");
    let actual = run(
        &mut client,
        "{ author_totals(min_articles: 2, order_by: {author_id: asc}) { author_id articles first_title author { name } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('author_id', t.author_id, 'articles', t.articles, 'first_title', t.first_title, 'author', json_build_object('name', a.name)) ORDER BY t.author_id) FROM \"author totals\"(2) t JOIN author a ON a.id = t.author_id",
    );
    assert_eq!(actual, expected);
    let actual = run(
        &mut client,
        "{ author(where: {latest: {title: {_like: \"title 1%\"}}}, order_by: {id: asc}) { id latest { title } } }",
        &admin,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('id', a.id, 'latest', json_build_object('title', l.title)) ORDER BY a.id) FROM author a JOIN latest_titles() l ON l.author_id = a.id WHERE l.title LIKE 'title 1%'",
    );
    assert_eq!(actual, expected);
    assert!(!actual.as_array().unwrap().is_empty());
}

#[test]
fn function_arguments_are_named_parameters_in_declared_order() {
    let schema = schema();
    let admin = Session::new("admin");
    let compile = |query: &str| {
        schema
            .compile(query, &json!({}), None, &admin)
            .unwrap()
            .remove(0)
            .compiled
    };
    let first = compile("{ search_articles(since_id: 4, search: \"a'b\") { id } }");
    let second = compile("{ search_articles(search: \"x\") { id } }");
    assert_eq!(first.params, vec![Value::Text("a'b".into()), Value::Int(4)]);
    assert_eq!(second.params, vec![Value::Text("x".into()), Value::Null]);
    assert_eq!(first.sql, second.sql);
    assert!(
        first
            .sql
            .contains("\"public\".\"search_articles\" ( \"search\" => $1 , \"since_id\" => $2 )")
    );
    let quoted = compile("{ author_totals(min_articles: 1) { author_id } }");
    assert!(
        quoted
            .sql
            .contains("\"public\".\"author totals\" ( \"min_articles\" => $1 )")
    );
    assert_eq!(quoted.params, vec![Value::Int(1)]);
}

#[test]
fn function_rows_keep_the_permissions_of_their_type() {
    let mut client = client();
    let user = Session::new("user").variable("user_id", Value::Int(3));
    let actual = run(
        &mut client,
        "{ search_articles(search: \"title%\", order_by: {id: desc}) { id title } }",
        &user,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('id', id, 'title', title) ORDER BY id DESC) FROM (SELECT * FROM search_articles('title%', NULL) WHERE author_id = 3 ORDER BY id DESC LIMIT 3) s",
    );
    assert_eq!(actual, expected);
    let actual = run(
        &mut client,
        "{ author_totals(min_articles: 2, order_by: {author_id: asc}) { author_id articles } }",
        &user,
    );
    let expected = plain(
        &mut client,
        "SELECT json_agg(json_build_object('author_id', author_id, 'articles', articles) ORDER BY author_id) FROM \"author totals\"(2) WHERE author_id <> 1",
    );
    assert_eq!(actual, expected);
    let schema = schema();
    for query in [
        "{ search_articles(search: \"x\") { comments { id } } }",
        "{ author_totals(min_articles: 1) { first_title } }",
    ] {
        assert!(
            schema.compile(query, &json!({}), None, &user).is_err(),
            "{query}"
        );
    }
    assert!(
        !schema
            .expanded_sdl("nobody")
            .unwrap()
            .contains("search_articles")
    );
    let sdl = schema.expanded_sdl("admin").unwrap();
    assert!(
        sdl.contains("search_articles(search: String!, since_id: Int, where: article_bool_exp")
    );
    assert!(sdl.contains("insert_article("));
    assert!(!sdl.contains("insert_author_totals"));
    assert!(!sdl.contains("insert_latest_titles"));
}

#[test]
fn function_declarations_are_checked() {
    let base = include_str!("fixtures/functions.graphql");
    for (from, to) in [
        (
            "latest: latest_titles @relation",
            "totals: [author_totals!]! @relation(columns: [\"id\"], references: [\"author_id\"])\n  latest: latest_titles @relation",
        ),
        (
            "latest_titles: [latest_titles!]! @list",
            "latest_titles: [latest_titles!]! @list @function(name: \"latest_titles\")",
        ),
        (
            "type latest_titles @function(",
            "type latest_titles @table(name: \"article\") @function(",
        ),
        (
            "arguments: {min_articles: \"Int!\"}",
            "arguments: {limit: \"Int!\"}",
        ),
        (
            "@function(name: \"latest_titles\", schema: \"public\")",
            "@function(name: \"\")",
        ),
    ] {
        let sdl = base.replacen(from, to, 1);
        assert_ne!(sdl, base, "{from}");
        assert!(Schema::from_sdl(&sdl).is_err(), "{to}");
    }
}

#[test]
fn lower_level_functions_take_their_declared_arguments() {
    let mut client = client();
    let article = Table::new("article", "public", "article")
        .column("id", "id")
        .column("title", "title")
        .relation(Relation::new(
            "author",
            Kind::Object,
            "author",
            ["author_id"],
            ["id"],
        ));
    let author = Table::new("author", "public", "author").column("name", "name");
    let totals = Table::function("totals", "public", "author totals", ["min_articles"])
        .column("author_id", "author_id")
        .column("articles", "articles");
    let search = Body::Function {
        schema: "public".into(),
        name: "search_articles".into(),
        arguments: vec!["search".into(), "since_id".into()],
    };
    let schema = Schema::new(
        vec![article, author, totals],
        vec![
            RootField::new("search", "article", RootKind::List).body(search),
            RootField::new("totals", "totals", RootKind::List),
        ],
    )
    .unwrap();
    let admin = Session::new("admin");
    let compiled = schema
        .compile_selection(
            &Selection::new("search")
                .argument("search", Value::Text("title 1%".into()))
                .argument("since_id", Value::Int(11))
                .order_by("id", rusuna::Direction::Asc)
                .field(Selection::new("id"))
                .field(Selection::new("author").field(Selection::new("name"))),
            &admin,
        )
        .unwrap();
    assert_eq!(
        fetch(&mut client, &compiled),
        plain(
            &mut client,
            "SELECT json_agg(json_build_object('id', s.id, 'author', json_build_object('name', a.name)) ORDER BY s.id) FROM search_articles('title 1%', 11) s JOIN author a ON a.id = s.author_id"
        )
    );
    for selection in [
        Selection::new("search").field(Selection::new("id")),
        Selection::new("totals")
            .argument("min", Value::Int(1))
            .field(Selection::new("articles")),
    ] {
        assert!(matches!(
            schema.compile_selection(&selection, &admin),
            Err(Error::InvalidArguments { .. })
        ));
    }
    let invalid = Body::Function {
        schema: "public".into(),
        name: "f".into(),
        arguments: vec!["offset".into()],
    };
    assert!(matches!(
        Schema::new(
            vec![Table::new("article", "public", "article").column("id", "id")],
            vec![RootField::new("f", "article", RootKind::List).body(invalid)],
        ),
        Err(Error::InvalidStatement { .. })
    ));
}
