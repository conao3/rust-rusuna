mod common;

use rusuna::{Direction, Error, Filter, Selection, Value};
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
        .filter(Filter::And(vec![
            Filter::eq("author_id", Value::Int(3)),
            Filter::eq("title", Value::Text("x' OR 1=1 --".into())),
        ]))
        .field(f("id"))
        .field(
            f("comments")
                .filter(Filter::eq(
                    "body",
                    Value::Text("\"; DROP TABLE comment".into()),
                ))
                .field(f("id")),
        )
        .field(
            f("author").field(
                f("articles")
                    .filter(Filter::eq("id", Value::Int(7)))
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
            .filter(Filter::eq("author_id", Value::Int(id)))
            .field(f("id"))
            .field(
                f("comments")
                    .filter(Filter::eq("body", Value::Text(body.into())))
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
            Error::UnknownRoot {
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
            f("article")
                .filter(Filter::eq("missing", Value::Int(1)))
                .field(f("id")),
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

#[test]
fn filters_keep_values_in_depth_first_document_order() {
    let query = f("author")
        .filter(Filter::And(vec![
            Filter::eq("id", Value::Int(3)),
            Filter::Relation {
                field: "articles".into(),
                filter: Box::new(Filter::Or(vec![
                    Filter::In {
                        field: "id".into(),
                        negated: false,
                        values: vec![Value::Int(4), Value::Int(5)],
                    },
                    Filter::Not(Box::new(Filter::eq("title", Value::Text("'\\\"".into())))),
                ])),
            },
            Filter::compare("rating", rusuna::Comparison::Gt, Value::Float(1.25)),
        ]))
        .field(
            f("articles_aggregate")
                .filter(Filter::eq("id", Value::Int(9)))
                .field(
                    f("nodes").field(
                        f("comments")
                            .filter(Filter::eq("id", Value::Int(10)))
                            .field(f("id")),
                    ),
                ),
        );
    let compiled = common::schema().compile(&query).unwrap();
    assert_eq!(
        compiled.params,
        vec![
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
            Value::Text("'\\\"".into()),
            Value::Float(1.25),
            Value::Int(9),
            Value::Int(10)
        ]
    );
    assert!(!compiled.sql.contains("1.25"));
    assert_eq!(compiled.sql.matches("EXISTS").count(), 1);
}

#[test]
fn null_and_nonfinite_comparisons_are_rejected() {
    for value in [
        Value::Null,
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
    ] {
        assert_eq!(
            common::schema().compile(
                &f("author")
                    .filter(Filter::eq("rating", value.clone()))
                    .field(f("id"))
            ),
            Err(Error::InvalidFilter)
        );
        assert_eq!(
            common::schema().compile(
                &f("author")
                    .filter(Filter::In {
                        field: "rating".into(),
                        negated: false,
                        values: vec![value]
                    })
                    .field(f("id"))
            ),
            Err(Error::InvalidFilter)
        );
    }
    for value in [
        0.0,
        -0.0,
        f64::MIN_POSITIVE,
        f64::MAX,
        -f64::MAX,
        f64::from_bits(1),
    ] {
        let out = common::schema()
            .compile(
                &f("author")
                    .filter(Filter::eq("rating", Value::Float(value)))
                    .field(f("id")),
            )
            .unwrap();
        let Value::Float(actual) = out.params[0] else {
            panic!("expected float")
        };
        assert_eq!(actual.to_bits(), value.to_bits());
    }
}

#[test]
fn filters_at_the_limits_compile_on_a_small_stack() {
    in_small_stack(|| {
        let schema = common::schema();
        let mut filter = Filter::In {
            field: "id".into(),
            negated: false,
            values: (0..rusuna::MAX_IN_VALUES)
                .map(|v| Value::Int(v as i64))
                .collect(),
        };
        for _ in 0..rusuna::MAX_FILTER_DEPTH {
            filter = Filter::Not(Box::new(filter));
        }
        assert_eq!(
            schema
                .compile(&f("author").filter(filter).field(f("id")))
                .unwrap()
                .params
                .len(),
            rusuna::MAX_IN_VALUES
        );
        let wide = Filter::And(
            (0..rusuna::MAX_FIELDS)
                .map(|v| Filter::eq("id", Value::Int(v as i64)))
                .collect(),
        );
        assert_eq!(
            schema
                .compile(&f("author").filter(wide).field(f("id")))
                .unwrap()
                .params
                .len(),
            rusuna::MAX_FIELDS
        );
        let too_many = Filter::In {
            field: "id".into(),
            negated: true,
            values: vec![Value::Int(1); rusuna::MAX_IN_VALUES + 1],
        };
        assert_eq!(
            schema.compile(&f("author").filter(too_many).field(f("id"))),
            Err(Error::InvalidFilter)
        );
        let too_wide = Filter::Or(vec![Filter::And(vec![]); rusuna::MAX_FIELDS + 1]);
        assert_eq!(
            schema.compile(&f("author").filter(too_wide).field(f("id"))),
            Err(Error::InvalidFilter)
        );
        let too_deep = (0..1000).fold(Filter::And(vec![]), |f, _| Filter::Not(Box::new(f)));
        assert_eq!(
            schema.compile(&f("author").filter(too_deep).field(f("id"))),
            Err(Error::TooDeep)
        );
    });
}

#[test]
fn root_names_and_type_names_are_independent_and_literals_are_quoted() {
    use rusuna::{RootField, RootKind, Schema, Table};
    let schema = Schema::new(
        vec![Table::new("Author's\\type", "public", "author").column("id", "id")],
        vec![RootField::new("people", "Author's\\type", RootKind::List)],
    );
    let compiled = schema.compile(&f("people").field(f("__typename"))).unwrap();
    assert!(compiled.sql.contains("'Author''s\\type'"));
    assert!(compiled.params.is_empty());
    assert_eq!(
        schema.compile(&f("author").field(f("id"))),
        Err(Error::UnknownRoot {
            name: "author".into()
        })
    );
}

#[test]
fn aggregate_selections_validate_arguments_and_emit_only_requested_functions() {
    let schema = common::schema();
    for name in ["aggregate_nodes", "aggregate_metrics"] {
        let case = common::cases()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap();
        assert!(!schema.compile(&case.query).unwrap().sql.contains("count("));
    }
    for query in [
        f("author_aggregate").field(f("id")),
        f("author_aggregate").field(f("aggregate").field(f("missing"))),
        f("author_aggregate").field(f("aggregate").field(f("count").limit(1))),
        f("author_aggregate").field(f("nodes").offset(1).field(f("id"))),
        f("author_aggregate").field(f("aggregate").field(f("sum").field(f("articles")))),
        f("article").field(f("author").offset(0).field(f("id"))),
        f("article").field(f("__typename").filter(Filter::And(vec![]))),
        f("author")
            .filter(Filter::Relation {
                field: "missing".into(),
                filter: Box::new(Filter::And(vec![])),
            })
            .field(f("id")),
        f("author")
            .filter(Filter::Relation {
                field: "articles_aggregate".into(),
                filter: Box::new(Filter::And(vec![])),
            })
            .field(f("id")),
    ] {
        assert!(schema.compile(&query).is_err(), "{query:?}");
    }
}

#[test]
fn nested_relations_with_a_maximum_filter_compile_on_a_small_stack() {
    in_small_stack(|| {
        let mut filter = Filter::In {
            field: "id".into(),
            negated: false,
            values: vec![Value::Int(1); rusuna::MAX_IN_VALUES],
        };
        for _ in 1..rusuna::MAX_FILTER_DEPTH {
            filter = Filter::Not(Box::new(filter));
        }
        let mut filters = vec![filter];
        filters.extend((1..rusuna::MAX_FIELDS).map(|_| Filter::eq("id", Value::Int(1))));
        let mut selection = f("articles").filter(Filter::And(filters)).field(f("id"));
        for level in (1..rusuna::MAX_DEPTH).rev() {
            selection = f(if level % 2 == 1 { "author" } else { "articles" }).field(selection);
        }
        let compiled = common::schema()
            .compile(&f("article").field(selection))
            .unwrap();
        assert_eq!(
            compiled.params.len(),
            rusuna::MAX_IN_VALUES + rusuna::MAX_FIELDS - 1
        );
    });
}
