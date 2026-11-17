mod common;

use rusuna::{Direction, Error, Selection, Value};
use std::path::PathBuf;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

fn f(name: &str) -> Selection {
    Selection::new(name)
}

fn in_small_stack(run: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn compiled_sql_matches_the_committed_statements() {
    let schema = common::schema();
    for case in common::cases() {
        let compiled = schema.compile(&case.query).unwrap();
        let path = fixture(&format!("expected/{}.sql", case.name));
        if std::env::var_os("RUSUNA_UPDATE_EXPECTED").is_some() {
            std::fs::write(&path, format!("{}\n", compiled.sql)).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            format!("{}\n", compiled.sql),
            expected,
            "query {}",
            case.name
        );
    }
}

#[test]
fn values_become_parameters_in_document_order() {
    let schema = common::schema();
    let query = f("article")
        .filter("author_id", Value::Int(3))
        .filter("title", Value::Text("x' OR 1=1 --".into()))
        .field(f("id"))
        .field(
            f("comments")
                .filter("body", Value::Text("\"; DROP TABLE comment".into()))
                .field(f("id")),
        )
        .field(
            f("author").field(
                f("articles")
                    .filter("id", Value::Int(7))
                    .limit(1)
                    .field(f("id")),
            ),
        );
    let compiled = schema.compile(&query).unwrap();
    assert_eq!(
        compiled.params,
        vec![
            Value::Int(3),
            Value::Text("x' OR 1=1 --".into()),
            Value::Text("\"; DROP TABLE comment".into()),
            Value::Int(7),
        ]
    );
    for index in 1..=4 {
        assert_eq!(compiled.sql.matches(&format!("${index}")).count(), 1);
    }
    assert!(!compiled.sql.contains("DROP") && !compiled.sql.contains("OR 1=1"));
}

#[test]
fn the_statement_does_not_depend_on_the_values() {
    let schema = common::schema();
    let query = |id, body: &str| {
        f("article")
            .filter("author_id", Value::Int(id))
            .field(f("id"))
            .field(
                f("comments")
                    .filter("body", Value::Text(body.into()))
                    .field(f("id")),
            )
    };
    let first = schema.compile(&query(1, "a")).unwrap();
    let second = schema.compile(&query(2, "b\"'")).unwrap();
    assert_eq!(first.sql, second.sql);
    assert_ne!(first.params, second.params);
}

#[test]
fn aliases_are_quoted_identifiers() {
    let schema = common::schema();
    let compiled = schema
        .compile(&f("author").field(f("name").alias("a\"b; DROP TABLE author")))
        .unwrap();
    assert!(compiled.sql.contains(r#"AS "a""b; DROP TABLE author""#));
}

#[test]
fn invalid_queries_are_rejected() {
    let schema = common::schema();
    let table = || "article".to_string();
    let cases = [
        (
            f("missing").field(f("id")),
            Error::UnknownType {
                name: "missing".into(),
            },
        ),
        (
            f("article").field(f("missing")),
            Error::UnknownField {
                table: table(),
                field: "missing".into(),
            },
        ),
        (
            f("article").filter("missing", Value::Int(1)).field(f("id")),
            Error::UnknownField {
                table: table(),
                field: "missing".into(),
            },
        ),
        (
            f("article")
                .order_by("comments", Direction::Asc)
                .field(f("id")),
            Error::UnknownField {
                table: table(),
                field: "comments".into(),
            },
        ),
        (
            f("article").field(f("id").limit(1)),
            Error::ColumnArguments {
                table: table(),
                field: "id".into(),
            },
        ),
        (
            f("article").field(f("id").field(f("id"))),
            Error::ColumnArguments {
                table: table(),
                field: "id".into(),
            },
        ),
        (
            f("article").field(f("author").limit(1).field(f("id"))),
            Error::ObjectArguments {
                table: table(),
                field: "author".into(),
            },
        ),
        (f("article"), Error::InvalidSelection { table: table() }),
        (
            f("article").field(f("author")),
            Error::InvalidSelection {
                table: "author".into(),
            },
        ),
        (
            f("article").field(f("id")).field(f("title").alias("id")),
            Error::InvalidSelection { table: table() },
        ),
        (
            f("article").field(f("id").alias("%")),
            Error::InvalidSelection { table: table() },
        ),
        (
            f("article").field(f("id").alias("")),
            Error::InvalidSelection { table: table() },
        ),
        (
            f("article").field(f("id").alias("x".repeat(64))),
            Error::InvalidSelection { table: table() },
        ),
    ];
    for (query, error) in cases {
        assert_eq!(schema.compile(&query), Err(error), "{query:?}");
    }
    let wide = (0..=rusuna::MAX_FIELDS).fold(f("article"), |query, index| {
        query.field(f("id").alias(format!("f{index}")))
    });
    assert_eq!(
        schema.compile(&wide),
        Err(Error::InvalidSelection { table: table() })
    );
}

#[test]
fn the_largest_queries_compile_on_a_small_stack() {
    in_small_stack(|| {
        let schema = common::schema();
        let mut inner: Option<Selection> = None;
        for level in (1..=rusuna::MAX_DEPTH).rev() {
            let relation = f(if level % 2 == 1 { "author" } else { "articles" }).field(f("id"));
            inner = Some(match inner {
                Some(inner) => relation.field(inner),
                None => relation,
            });
        }
        let root = f("article").field(f("id")).field(inner.unwrap());
        let wide = (0..rusuna::MAX_FIELDS - 2).fold(root, |query, index| {
            query.field(f("id").alias(format!("f{index}")))
        });
        let compiled = schema.compile(&wide).unwrap();
        assert!(compiled.sql.len() > 10_000);
        let too_deep = (0..1_000).fold(f("id"), |inner, _| f("author").field(inner));
        assert_eq!(
            schema.compile(&f("article").field(too_deep)),
            Err(Error::TooDeep)
        );
    });
}

#[test]
fn compiles_from_several_threads() {
    let schema = common::schema();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for case in common::cases() {
                    schema.compile(&case.query).unwrap();
                }
            });
        }
    });
}
