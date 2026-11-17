#[path = "../../tests/common/mod.rs"]
mod common;

use serde_json::Value as Json;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::path::Path;
use std::time::Instant;
use viat_gql::{Engine, Schema};

const RUNS: usize = 15;

struct Times {
    median: f64,
    min: f64,
    max: f64,
}

fn times(mut samples: Vec<f64>) -> Times {
    samples.sort_by(f64::total_cmp);
    Times {
        median: samples[samples.len() / 2],
        min: samples[0],
        max: samples[samples.len() - 1],
    }
}

fn sorted(value: Json, unordered: &[&str]) -> Json {
    match value {
        Json::Array(items) => Json::Array(
            items
                .into_iter()
                .map(|item| sorted(item, unordered))
                .collect(),
        ),
        Json::Object(fields) => Json::Object(
            fields
                .into_iter()
                .map(|(key, value)| match sorted(value, unordered) {
                    Json::Array(mut items) if unordered.contains(&key.as_str()) => {
                        items.sort_by_key(|item| item.to_string());
                        (key, Json::Array(items))
                    }
                    value => (key, value),
                })
                .collect(),
        ),
        value => value,
    }
}

async fn run(pool: &PgPool, sql: &str) -> anyhow::Result<(Json, f64)> {
    let start = Instant::now();
    let value: Json = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await?;
    Ok((value, start.elapsed().as_secs_f64() * 1000.0))
}

fn options(config: &str) -> anyhow::Result<PgConnectOptions> {
    let field = |name: &str| {
        config
            .split(' ')
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .ok_or_else(|| anyhow::anyhow!("RUSUNA_POSTGRES has no `{name}`"))
    };
    Ok(PgConnectOptions::new()
        .socket(field("host")?)
        .username(field("user")?)
        .database(field("dbname")?))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = std::env::var("RUSUNA_POSTGRES")
        .map_err(|_| anyhow::anyhow!("RUSUNA_POSTGRES is not set; run `make bench`"))?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options(&config)?)
        .await?;
    let schema = common::schema();
    let viat = Engine::new(pool.clone(), Schema::introspect(&pool).await?.build());
    println!("query\tstatement\tmedian_ms\tmin_ms\tmax_ms\truns");
    let mut verdicts = Vec::new();
    for case in common::cases().into_iter().take(3) {
        let name = case.name;
        let start = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(schema.compile(&case.query)?);
        }
        let compile_us = start.elapsed().as_secs_f64() * 1000.0;
        let compiled = schema.compile(&case.query)?;
        anyhow::ensure!(compiled.params.is_empty(), "query {name} has parameters");
        let hasura =
            std::fs::read_to_string(root.join(format!("../tests/fixtures/hasura/{name}.sql")))?;
        let hand = std::fs::read_to_string(root.join(format!("hand/{name}.sql")))?;
        let graphql = std::fs::read_to_string(root.join(format!("queries/viat-{name}.graphql")))?;
        let viat_compiled = viat.compile(&graphql)?;
        let mut samples = [const { Vec::new() }; 4];
        let mut results = Vec::new();
        for _ in 0..RUNS {
            let (rusuna, t) = run(&pool, &compiled.sql).await?;
            samples[0].push(t);
            let start = Instant::now();
            let viat_result = viat.execute(&viat_compiled, None).await?;
            samples[1].push(start.elapsed().as_secs_f64() * 1000.0);
            let (hasura_result, t) = run(&pool, &hasura).await?;
            samples[2].push(t);
            let (hand_result, t) = run(&pool, &hand).await?;
            samples[3].push(t);
            let viat_rows = viat_result
                .get(
                    viat_result
                        .as_object()
                        .and_then(|o| o.keys().next())
                        .unwrap(),
                )
                .cloned()
                .unwrap();
            results = vec![rusuna, viat_rows, hasura_result, hand_result];
        }
        let expected = sorted(results[2].clone(), case.unordered);
        for (label, result) in ["rusuna", "viat-gql", "hasura", "hand"]
            .iter()
            .zip(&results)
        {
            anyhow::ensure!(
                sorted(result.clone(), case.unordered) == expected,
                "query {name}: the result of {label} differs from the result of the Hasura statement"
            );
        }
        let [rusuna, viat_times, hasura_times, hand_times] = samples.map(times);
        for (label, t) in [
            ("rusuna", &rusuna),
            ("viat-gql", &viat_times),
            ("hasura", &hasura_times),
            ("hand", &hand_times),
        ] {
            println!(
                "{name}\t{label}\t{:.1}\t{:.1}\t{:.1}\t{RUNS}",
                t.median, t.min, t.max
            );
        }
        println!(
            "{name}\trusuna-compile\t{:.4}\t\t\t1000",
            compile_us / 1000.0
        );
        let met = if name == "c" {
            rusuna.median <= 1.5 * hand_times.median
        } else {
            hasura_times.min <= rusuna.median && rusuna.median <= hasura_times.max
        };
        let faster = name != "c" && rusuna.median < hasura_times.min;
        verdicts.push(format!(
            "{name}\t{}",
            if met {
                "goal met"
            } else if faster {
                "faster than every run of the Hasura statement"
            } else {
                "goal missed"
            }
        ));
    }
    println!("results\tequal for rusuna, viat-gql, hasura and hand");
    for verdict in verdicts {
        println!("{verdict}");
    }
    Ok(())
}
