use clap::{Parser, Subcommand, ValueEnum};
use rusuna::{Schema, Session, Value};
use serde_json::{Value as Json, json};
use std::io::{self, Write};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "rusuna",
    version,
    about = "Compile GraphQL to PostgreSQL",
    propagate_version = true
)]
struct Cli {
    #[arg(long, global = true, value_enum, default_value = "json")]
    output: Output,
    #[arg(long, global = true, overrides_with = "no_debug")]
    debug: bool,
    #[arg(long, global = true, overrides_with = "debug")]
    no_debug: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Output {
    Json,
    Text,
    Table,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Inspect schema definitions")]
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    #[command(about = "Compile query documents")]
    Query {
        #[command(subcommand)]
        command: QueryCommand,
    },
}

#[derive(Subcommand)]
enum SchemaCommand {
    #[command(about = "Expand mapping SDL into a query schema")]
    ExpandSdl {
        #[arg(long)]
        role: String,
        #[arg(long, value_parser = input)]
        schema: String,
    },
}

#[derive(Subcommand)]
enum QueryCommand {
    #[command(about = "Compile each selected root field to SQL and parameters")]
    CompileSql {
        #[arg(long)]
        role: String,
        #[arg(long, default_value = "{}", value_parser = variables)]
        session: Json,
        #[arg(long, value_parser = input)]
        schema: String,
        #[arg(long, value_parser = input)]
        query: String,
        #[arg(long, value_parser = input)]
        operation_name: Option<String>,
        #[arg(long, default_value = "{}", value_parser = variables)]
        variables: Json,
    },
}

fn input(value: &str) -> Result<String, String> {
    match value.strip_prefix("file://") {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}")),
        None => Ok(value.to_string()),
    }
}

fn variables(value: &str) -> Result<Json, String> {
    let value: Json = serde_json::from_str(&input(value)?).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("variables must be a JSON object".into());
    }
    Ok(value)
}

fn parameter(value: Value) -> Json {
    match value {
        Value::Int(v) => json!(v),
        Value::Float(v) => json!(v),
        Value::Text(v) => json!(v),
        Value::Bool(v) => json!(v),
        Value::Null => Json::Null,
    }
}

fn execute(command: Command) -> Result<Json, rusuna::Error> {
    match command {
        Command::Schema {
            command: SchemaCommand::ExpandSdl { schema, role },
        } => Ok(json!({"Sdl": Schema::from_sdl(&schema)?.expanded_sdl(&role)?})),
        Command::Query {
            command:
                QueryCommand::CompileSql {
                    role,
                    session,
                    schema,
                    query,
                    operation_name,
                    variables,
                },
        } => {
            let fields = Schema::from_sdl(&schema)?.compile(
                &query,
                &variables,
                operation_name.as_deref(),
                &Session::from_json(role, &session)?,
            )?;
            Ok(
                json!({"Fields": fields.into_iter().map(|f| json!({"ResponseKey": f.response_key, "Sql": f.compiled.sql, "Params": f.compiled.params.into_iter().map(parameter).collect::<Vec<_>>()})).collect::<Vec<_>>()}),
            )
        }
    }
}

fn output(value: &Json, format: Output) -> String {
    match format {
        Output::Json => serde_json::to_string_pretty(value).unwrap(),
        Output::Text | Output::Table => {
            if let Some(sdl) = value.get("Sdl") {
                return sdl.as_str().unwrap().trim_end().into();
            }
            let rows = value["Fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    format!(
                        "{}\t{}\t{}",
                        f["ResponseKey"].as_str().unwrap(),
                        f["Sql"].as_str().unwrap(),
                        f["Params"]
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            if matches!(format, Output::Table) {
                format!("ResponseKey\tSql\tParams\n{rows}")
            } else {
                rows
            }
        }
    }
}

fn main() -> ExitCode {
    let mut args: Vec<_> = std::env::args_os().collect();
    if args.last().is_some_and(|v| v == "help")
        && args[1..args.len() - 1].iter().all(|v| {
            ["schema", "query", "expand-sdl", "compile-sql"]
                .iter()
                .any(|name| v == name)
        })
    {
        *args.last_mut().unwrap() = "--help".into();
    }
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 252 } else { 0 };
            if error.print().is_err() {
                return ExitCode::from(255);
            }
            return ExitCode::from(code);
        }
    };
    match execute(cli.command) {
        Ok(value) => {
            if writeln!(io::stdout().lock(), "{}", output(&value, cli.output)).is_err() {
                return ExitCode::from(255);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let message = if cli.debug {
                format!("{error:?}")
            } else {
                error.to_string()
            };
            if writeln!(io::stderr().lock(), "{}", json!({"Error": message})).is_err() {
                return ExitCode::from(255);
            }
            ExitCode::from(254)
        }
    }
}
