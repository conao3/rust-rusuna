use rusuna::{Comparison, Direction, Filter, Schema, Selection, Value};

#[allow(dead_code)]
pub struct Case {
    pub name: &'static str,
    pub query: Selection,
    /// Fields whose lists have no `order_by`, compared as multisets.
    pub unordered: &'static [&'static str],
}

pub fn schema() -> Schema {
    Schema::from_sdl(include_str!("../fixtures/schema.graphql")).unwrap()
}

fn f(name: &str) -> Selection {
    Selection::new(name)
}

/// The queries of `tests/fixtures/queries/`, in the same order.
pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "a",
            query: f("article")
                .limit(20)
                .order_by("published_at", Direction::Desc)
                .field(f("id"))
                .field(f("title"))
                .field(f("author").field(f("name")))
                .field(
                    f("comments")
                        .limit(5)
                        .order_by("id", Direction::Asc)
                        .field(f("body")),
                ),
            unordered: &[],
        },
        Case {
            name: "b",
            query: f("article")
                .limit(50000)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("title"))
                .field(f("author").field(f("name"))),
            unordered: &[],
        },
        Case {
            name: "c",
            query: f("author")
                .limit(2000)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("name"))
                .field(
                    f("articles")
                        .field(f("id"))
                        .field(f("title"))
                        .field(f("comments").field(f("body"))),
                ),
            unordered: &["articles", "comments"],
        },
        Case {
            name: "d",
            query: f("article")
                .filter(Filter::eq("author_id", Value::Int(3)))
                .order_by("id", Direction::Desc)
                .field(f("id"))
                .field(f("title"))
                .field(
                    f("comments")
                        .filter(Filter::eq("article_id", Value::Int(9)))
                        .field(f("id"))
                        .field(f("body")),
                )
                .field(
                    f("author").field(f("id")).field(f("name")).field(
                        f("articles")
                            .limit(2)
                            .order_by("id", Direction::Asc)
                            .field(f("id")),
                    ),
                ),
            unordered: &["comments"],
        },
        Case {
            name: "e",
            query: f("author")
                .order_by("id", Direction::Asc)
                .field(f("name"))
                .field(
                    f("articles")
                        .order_by("published_at", Direction::Desc)
                        .field(f("title"))
                        .field(
                            f("comments")
                                .limit(1)
                                .order_by("id", Direction::Desc)
                                .field(f("body")),
                        )
                        .field(f("author").field(f("name"))),
                ),
            unordered: &[],
        },
        Case {
            name: "f",
            query: f("comment")
                .filter(Filter::eq("body", Value::Text("it's \"quoted\"".into())))
                .order_by("id", Direction::Asc)
                .limit(3)
                .field(f("id"))
                .field(f("body").alias("text"))
                .field(
                    f("article")
                        .field(f("title").alias("headline"))
                        .field(f("author").field(f("name"))),
                ),
            unordered: &[],
        },
        Case {
            name: "g",
            query: f("article")
                .order_by("author_id", Direction::Desc)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("author_id"))
                .field(f("revisions").field(f("note")))
                .field(f("comments").field(f("id")))
                .field(
                    f("revisions")
                        .alias("latest")
                        .limit(1)
                        .order_by("note", Direction::Desc)
                        .field(f("note"))
                        .field(f("article").field(f("title"))),
                ),
            unordered: &["revisions", "comments"],
        },
        Case {
            name: "compare_neq",
            query: f("author")
                .filter(Filter::compare("id", Comparison::Neq, Value::Int(3)))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "compare_gt",
            query: f("author")
                .filter(Filter::compare("id", Comparison::Gt, Value::Int(3)))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "compare_gte",
            query: f("author")
                .filter(Filter::compare("id", Comparison::Gte, Value::Int(3)))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "compare_lt",
            query: f("author")
                .filter(Filter::compare("id", Comparison::Lt, Value::Int(3)))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "compare_lte",
            query: f("author")
                .filter(Filter::compare("id", Comparison::Lte, Value::Int(3)))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "boolean",
            query: f("author")
                .filter(Filter::And(vec![
                    Filter::Or(vec![
                        Filter::eq("id", Value::Int(1)),
                        Filter::eq("id", Value::Int(3)),
                    ]),
                    Filter::Not(Box::new(Filter::eq("name", Value::Text("absent".into())))),
                ]))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "in_empty",
            query: f("author")
                .filter(Filter::In {
                    field: "id".into(),
                    negated: false,
                    values: vec![],
                })
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "in_values",
            query: f("author")
                .filter(Filter::In {
                    field: "id".into(),
                    negated: false,
                    values: vec![Value::Int(1), Value::Int(3), Value::Int(6)],
                })
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "nin_empty",
            query: f("author")
                .filter(Filter::In {
                    field: "id".into(),
                    negated: true,
                    values: vec![],
                })
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "nin_values",
            query: f("author")
                .filter(Filter::In {
                    field: "id".into(),
                    negated: true,
                    values: vec![Value::Int(1), Value::Int(3), Value::Int(6)],
                })
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "like",
            query: f("author")
                .filter(Filter::compare(
                    "nickname",
                    Comparison::Like,
                    Value::Text("A%".into()),
                ))
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "nlike",
            query: f("author")
                .filter(Filter::compare(
                    "nickname",
                    Comparison::Nlike,
                    Value::Text("A%".into()),
                ))
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "ilike",
            query: f("author")
                .filter(Filter::compare(
                    "nickname",
                    Comparison::Ilike,
                    Value::Text("A%".into()),
                ))
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "nilike",
            query: f("author")
                .filter(Filter::compare(
                    "nickname",
                    Comparison::Nilike,
                    Value::Text("A%".into()),
                ))
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "is_null_true",
            query: f("author")
                .filter(Filter::IsNull {
                    field: "nickname".into(),
                    value: true,
                })
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "is_null_false",
            query: f("author")
                .filter(Filter::IsNull {
                    field: "nickname".into(),
                    value: false,
                })
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "order_asc",
            query: f("author")
                .order_by("nickname", Direction::Asc)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "order_asc_nulls_first",
            query: f("author")
                .order_by("nickname", Direction::AscNullsFirst)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "order_asc_nulls_last",
            query: f("author")
                .order_by("nickname", Direction::AscNullsLast)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "order_desc",
            query: f("author")
                .order_by("nickname", Direction::Desc)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "order_desc_nulls_first",
            query: f("author")
                .order_by("nickname", Direction::DescNullsFirst)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "order_desc_nulls_last",
            query: f("author")
                .order_by("nickname", Direction::DescNullsLast)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("nickname")),
            unordered: &[],
        },
        Case {
            name: "float",
            query: f("author")
                .filter(Filter::compare(
                    "rating",
                    Comparison::Gte,
                    Value::Float(1.25),
                ))
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("rating")),
            unordered: &[],
        },
        Case {
            name: "object_filter",
            query: f("article")
                .filter(Filter::Relation {
                    field: "author".into(),
                    filter: Box::new(Filter::eq("name", Value::Text("author 3".into()))),
                })
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("author").field(f("name"))),
            unordered: &[],
        },
        Case {
            name: "array_filter",
            query: f("author")
                .filter(Filter::Relation {
                    field: "articles".into(),
                    filter: Box::new(Filter::Relation {
                        field: "comments".into(),
                        filter: Box::new(Filter::compare(
                            "body",
                            Comparison::Ilike,
                            Value::Text("%quoted%".into()),
                        )),
                    }),
                })
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "composite_filter",
            query: f("revision")
                .filter(Filter::Relation {
                    field: "article".into(),
                    filter: Box::new(Filter::eq("id", Value::Int(1))),
                })
                .order_by("note", Direction::Asc)
                .field(f("note")),
            unordered: &[],
        },
        Case {
            name: "empty_boolean",
            query: f("author")
                .filter(Filter::And(vec![
                    Filter::And(vec![]),
                    Filter::Not(Box::new(Filter::Or(vec![]))),
                    Filter::Relation {
                        field: "articles".into(),
                        filter: Box::new(Filter::And(vec![])),
                    },
                ]))
                .order_by("id", Direction::Asc)
                .field(f("id")),
            unordered: &[],
        },
        Case {
            name: "offset",
            query: f("author")
                .offset(1)
                .limit(4)
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(
                    f("articles")
                        .offset(1)
                        .order_by("id", Direction::Asc)
                        .field(f("id")),
                )
                .field(
                    f("articles")
                        .alias("page")
                        .offset(1)
                        .limit(1)
                        .order_by("id", Direction::Desc)
                        .field(f("id")),
                ),
            unordered: &[],
        },
        Case {
            name: "typename",
            query: f("article")
                .limit(2)
                .order_by("id", Direction::Asc)
                .field(f("__typename"))
                .field(f("id"))
                .field(
                    f("author")
                        .field(f("__typename").alias("kind"))
                        .field(f("name")),
                )
                .field(f("comments").field(f("__typename")).field(f("id"))),
            unordered: &["comments"],
        },
        Case {
            name: "aggregate_root",
            query: f("author_aggregate")
                .filter(Filter::compare("id", Comparison::Gte, Value::Int(2)))
                .order_by("id", Direction::Desc)
                .limit(3)
                .offset(1)
                .field(
                    f("aggregate")
                        .field(f("count"))
                        .field(f("sum").field(f("id")))
                        .field(f("avg").field(f("id")))
                        .field(f("min").field(f("id")))
                        .field(f("max").field(f("id"))),
                )
                .field(
                    f("nodes")
                        .field(f("id"))
                        .field(f("name"))
                        .field(f("articles").field(f("id"))),
                ),
            unordered: &["articles"],
        },
        Case {
            name: "aggregate_array",
            query: f("author")
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(
                    f("articles_aggregate")
                        .field(
                            f("aggregate")
                                .field(f("count"))
                                .field(f("sum").field(f("id")))
                                .field(f("avg").field(f("id")))
                                .field(f("min").field(f("id")))
                                .field(f("max").field(f("id"))),
                        )
                        .field(f("nodes").field(f("id")).field(
                            f("comments_aggregate").field(f("aggregate").field(f("count"))),
                        )),
                )
                .field(
                    f("articles_aggregate")
                        .alias("page")
                        .order_by("id", Direction::Desc)
                        .limit(2)
                        .offset(1)
                        .field(
                            f("aggregate")
                                .field(f("count"))
                                .field(f("sum").field(f("id")))
                                .field(f("avg").field(f("id")))
                                .field(f("min").field(f("id")))
                                .field(f("max").field(f("id"))),
                        )
                        .field(f("nodes").field(f("id"))),
                ),
            unordered: &["nodes"],
        },
        Case {
            name: "aggregate_empty",
            query: f("article_aggregate")
                .filter(Filter::compare("id", Comparison::Lt, Value::Int(0)))
                .field(
                    f("aggregate")
                        .field(f("count"))
                        .field(f("sum").field(f("id")))
                        .field(f("avg").field(f("id")))
                        .field(f("min").field(f("id")))
                        .field(f("max").field(f("id"))),
                )
                .field(f("nodes").field(f("id"))),
            unordered: &[],
        },
        Case {
            name: "aggregate_nodes",
            query: f("article_aggregate")
                .order_by("id", Direction::Asc)
                .limit(3)
                .field(
                    f("nodes")
                        .field(f("id"))
                        .field(f("author").field(f("name"))),
                )
                .field(
                    f("nodes")
                        .alias("other")
                        .field(f("title"))
                        .field(f("comments").field(f("id"))),
                ),
            unordered: &["comments"],
        },
        Case {
            name: "aggregate_metrics",
            query: f("author_aggregate").field(
                f("aggregate")
                    .field(f("sum").field(f("rating")))
                    .field(f("avg").field(f("rating")))
                    .field(f("min").field(f("name")).field(f("nickname")))
                    .field(f("max").field(f("name")).field(f("nickname"))),
            ),
            unordered: &[],
        },
        Case {
            name: "custom_root",
            query: f("people")
                .order_by("id", Direction::Asc)
                .field(f("id"))
                .field(f("__typename")),
            unordered: &[],
        },
        Case {
            name: "custom_aggregate_root",
            query: f("people_stats")
                .field(f("aggregate").field(f("count")))
                .field(f("nodes").field(f("__typename"))),
            unordered: &["nodes"],
        },
    ]
}
