use rusuna::{Direction, Kind, Relation, Schema, Selection, Table, Value};

#[allow(dead_code)]
pub struct Case {
    pub name: &'static str,
    pub query: Selection,
    /// Fields whose lists have no `order_by`, compared as multisets.
    pub unordered: &'static [&'static str],
}

pub fn schema() -> Schema {
    Schema::new(vec![
        Table::new("author", "public", "author")
            .column("id", "id")
            .column("name", "name")
            .relation(Relation::new(
                "articles",
                Kind::Array,
                "article",
                ["id"],
                ["author_id"],
            )),
        Table::new("article", "public", "article")
            .column("id", "id")
            .column("author_id", "author_id")
            .column("title", "title")
            .column("published_at", "published_at")
            .relation(Relation::new(
                "author",
                Kind::Object,
                "author",
                ["author_id"],
                ["id"],
            ))
            .relation(Relation::new(
                "comments",
                Kind::Array,
                "comment",
                ["id"],
                ["article_id"],
            ))
            .relation(Relation::new(
                "revisions",
                Kind::Array,
                "revision",
                ["id", "author_id"],
                ["article_id", "author_id"],
            )),
        Table::new("comment", "public", "comment")
            .column("id", "id")
            .column("article_id", "article_id")
            .column("body", "body")
            .relation(Relation::new(
                "article",
                Kind::Object,
                "article",
                ["article_id"],
                ["id"],
            )),
        Table::new("revision", "public", "revision")
            .column("article_id", "article_id")
            .column("author_id", "author_id")
            .column("note", "note")
            .relation(Relation::new(
                "article",
                Kind::Object,
                "article",
                ["article_id", "author_id"],
                ["id", "author_id"],
            )),
    ])
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
                .filter("author_id", Value::Int(3))
                .order_by("id", Direction::Desc)
                .field(f("id"))
                .field(f("title"))
                .field(
                    f("comments")
                        .filter("article_id", Value::Int(9))
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
                .filter("body", Value::Text("it's \"quoted\"".into()))
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
    ]
}
