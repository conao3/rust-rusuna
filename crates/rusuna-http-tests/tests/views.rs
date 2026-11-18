mod support;

use serde_json::{Value as Json, json};
use support::*;

const QUERY: &str = "{
  view_author(order_by: {id: asc}) {
    id
    published(order_by: {id: asc}) { id title author { name } }
    published_aggregate { aggregate { count sum { score } } }
    stats { articles total }
  }
  view_published_aggregate { aggregate { count max { score } } nodes { id } }
  view_author_stats(order_by: {author_id: asc}) { author_id articles total author { name } }
  view_author_stats_by_pk(author_id: 2) { total }
}";

async fn fields(address: std::net::SocketAddr, name: &str) -> Vec<String> {
    let response = post(
        address,
        &[],
        &json!({ "query": format!("{{ __type(name: \"{name}\") {{ fields {{ name }} }} }}") }),
    )
    .await;
    response["data"]["__type"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn views_and_materialized_views_are_read_with_relationships_and_aggregates() {
    let pool = database("http_views_read", &["views.sql"]).await;
    let address = serve(schemas(&fixture("views.graphql"), &pool, None)).await;
    let response = post(address, &[], &json!({ "query": QUERY })).await;
    assert_eq!(
        response,
        json!({"data": {
            "view_author": [
                {
                    "id": 1,
                    "published": [{"id": 1, "title": "first", "author": {"name": "Ada"}}],
                    "published_aggregate": {"aggregate": {"count": 1, "sum": {"score": 3}}},
                    "stats": {"articles": 2, "total": 8},
                },
                {
                    "id": 2,
                    "published": [
                        {"id": 3, "title": "second", "author": {"name": "Bea"}},
                        {"id": 4, "title": "third", "author": {"name": "Bea"}},
                    ],
                    "published_aggregate": {"aggregate": {"count": 2, "sum": {"score": 9}}},
                    "stats": {"articles": 2, "total": 9},
                },
                {
                    "id": 3,
                    "published": [],
                    "published_aggregate": {"aggregate": {"count": 0, "sum": {"score": null}}},
                    "stats": null,
                },
            ],
            "view_published_aggregate": {
                "aggregate": {"count": 3, "max": {"score": 7}},
                "nodes": [{"id": 1}, {"id": 3}, {"id": 4}],
            },
            "view_author_stats": [
                {"author_id": 1, "articles": 2, "total": 8, "author": {"name": "Ada"}},
                {"author_id": 2, "articles": 2, "total": 9, "author": {"name": "Bea"}},
            ],
            "view_author_stats_by_pk": {"total": 9},
        }})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn view_permissions_restrict_rows_columns_and_aggregates() {
    let pool = database("http_views_permissions", &["views.sql"]).await;
    let address = serve(schemas(&fixture("views.graphql"), &pool, None)).await;
    let query = "{
      view_author(order_by: {id: asc}) { id published { title } stats { total } }
      view_published_aggregate { aggregate { count } }
      view_author_stats { author_id total }
    }";
    for (user, expected) in [
        (
            1,
            json!({
                "view_author": [
                    {"id": 1, "published": [{"title": "first"}], "stats": null},
                    {"id": 2, "published": [], "stats": {"total": 9}},
                    {"id": 3, "published": [], "stats": null},
                ],
                "view_published_aggregate": {"aggregate": {"count": 1}},
                "view_author_stats": [{"author_id": 2, "total": 9}],
            }),
        ),
        (
            2,
            json!({
                "view_author": [
                    {"id": 1, "published": [], "stats": null},
                    {"id": 2, "published": [{"title": "second"}, {"title": "third"}], "stats": {"total": 9}},
                    {"id": 3, "published": [], "stats": null},
                ],
                "view_published_aggregate": {"aggregate": {"count": 2}},
                "view_author_stats": [{"author_id": 2, "total": 9}],
            }),
        ),
    ] {
        let response = post(
            address,
            &role_headers("user", &json!({"user_id": user})),
            &json!({ "query": query }),
        )
        .await;
        assert_eq!(
            sorted(response, &["published"]),
            sorted(json!({ "data": expected }), &["published"]),
            "{user}"
        );
    }
    let hidden = post(
        address,
        &role_headers("visitor", &json!({})),
        &json!({ "query": "{ view_published { id } }" }),
    )
    .await;
    assert_eq!(hidden["data"], Json::Null, "{hidden}");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_tables_have_no_mutations() {
    let pool = database("http_views_mutations", &["views.sql"]).await;
    let sdl = fixture("views.graphql");
    let address = serve(schemas(&sdl, &pool, None)).await;
    let mutations = fields(address, "Mutation").await;
    assert!(mutations.contains(&"insert_view_author".to_string()));
    assert!(mutations.contains(&"delete_view_author_by_pk".to_string()));
    assert!(
        mutations
            .iter()
            .all(|f| !f.contains("view_published") && !f.contains("view_author_stats")),
        "{mutations:?}"
    );
    let roots = fields(address, "Query").await;
    assert!(roots.contains(&"view_author_stats_by_pk".to_string()));
    assert!(!roots.iter().any(|f| f == "view_published_by_pk"));
    let expanded = rusuna::Schema::from_sdl(&sdl)
        .unwrap()
        .expanded_sdl("admin")
        .unwrap();
    for name in [
        "view_published_insert_input",
        "view_published_mutation_response",
        "view_author_stats_on_conflict",
        "view_author_stats_pk_columns_input",
    ] {
        assert!(!expanded.contains(name), "{name}");
    }

    let rejected = post(
        address,
        &[],
        &json!({ "query": "mutation { delete_view_published(where: {}) { affected_rows } }" }),
    )
    .await;
    assert_eq!(rejected["data"], Json::Null, "{rejected}");
    assert!(
        rejected["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("delete_view_published"),
        "{rejected}"
    );

    let schema = rusuna::Schema::from_sdl(&sdl).unwrap();
    let error = schema
        .compile_mutation(
            &rusuna::MutationArguments {
                filter: Some(rusuna::Filter::And(Vec::new())),
                ..rusuna::MutationArguments::new("view_published", rusuna::WriteKind::Delete)
            },
            &[rusuna::Selection::new("affected_rows")],
            &rusuna::Session::new("admin"),
        )
        .unwrap_err();
    assert_eq!(
        error,
        rusuna::Error::ReadOnly {
            table: "view_published".into()
        }
    );

    let written = sdl.replace(
        "type view_published @table(name: \"view_published\", read_only: true)",
        "type view_published @table(name: \"view_published\", read_only: true) @delete_permission(role: \"user\", where: {})",
    );
    assert!(rusuna::Schema::from_sdl(&written).is_err());
    let lower = rusuna::Schema::new(
        vec![
            rusuna::Table::new("v", "public", "v")
                .column("id", "id")
                .read_only(),
        ],
        Vec::new(),
    )
    .unwrap();
    assert!(
        lower
            .compile_mutation(
                &rusuna::MutationArguments::new("v", rusuna::WriteKind::Insert),
                &[rusuna::Selection::new("affected_rows")],
                &rusuna::Session::new("admin"),
            )
            .is_err()
    );
    let mut table = rusuna::Table::new("w", "public", "w")
        .column("id", "id")
        .read_only();
    table.write_permissions.push(rusuna::WritePermission {
        role: "user".into(),
        kind: rusuna::WriteKind::Delete,
        columns: Vec::new(),
        filter: rusuna::Filter::And(Vec::new()),
        check: rusuna::Filter::And(Vec::new()),
        presets: Vec::new(),
    });
    assert_eq!(
        rusuna::Schema::new(vec![table], Vec::new()).unwrap_err(),
        rusuna::Error::ReadOnly { table: "w".into() }
    );
}
