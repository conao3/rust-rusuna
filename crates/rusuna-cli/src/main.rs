use clap::{Parser, Subcommand, ValueEnum};
use rusuna::{Schema, Session, Value};
use rusuna_events::EventTrigger;
use serde_json::{Value as Json, json};
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
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
    #[command(about = "Compile query and mutation documents")]
    Query {
        #[command(subcommand)]
        command: QueryCommand,
    },
    #[command(about = "Generate the SQL of event triggers")]
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
}

#[derive(Subcommand)]
enum EventsCommand {
    #[command(
        about = "Print the migration that creates the event tables and the declared triggers"
    )]
    GenerateMigration {
        #[arg(long, default_value = "[]", value_parser = triggers)]
        triggers: Triggers,
    },
}

#[derive(Clone)]
struct Triggers(Vec<EventTrigger>);

#[derive(Subcommand)]
enum SchemaCommand {
    #[command(about = "Generate REST mapping SDL from an OpenAPI document")]
    ImportOpenapi {
        path: PathBuf,
        #[arg(long)]
        source: String,
        #[arg(long)]
        role: Vec<String>,
        #[arg(long)]
        include_tag: Vec<String>,
    },
    #[command(about = "Expand mapping SDL into a GraphQL schema")]
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

fn triggers(value: &str) -> Result<Triggers, String> {
    serde_json::from_str(&input(value)?)
        .map(Triggers)
        .map_err(|e| format!("triggers must be a JSON array of trigger objects: {e}"))
}

#[derive(Debug)]
enum Error {
    Schema(rusuna::Error),
    Events(rusuna_events::Error),
    OpenApi(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Schema(error) => error.fmt(f),
            Error::Events(error) => error.fmt(f),
            Error::OpenApi(error) => f.write_str(error),
        }
    }
}

impl From<rusuna::Error> for Error {
    fn from(error: rusuna::Error) -> Self {
        Error::Schema(error)
    }
}

impl From<rusuna_events::Error> for Error {
    fn from(error: rusuna_events::Error) -> Self {
        Error::Events(error)
    }
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

fn execute(command: Command) -> Result<Json, Error> {
    match command {
        Command::Schema {
            command:
                SchemaCommand::ImportOpenapi {
                    path,
                    source,
                    role,
                    include_tag,
                },
        } => rusuna_cli::import_openapi(&path, &source, &role, &include_tag)
            .map(|sdl| json!({"Sdl": sdl}))
            .map_err(Error::OpenApi),
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
        Command::Events {
            command: EventsCommand::GenerateMigration { triggers },
        } => Ok(json!({"Sql": rusuna_events::migration(&triggers.0)?})),
    }
}

fn output(value: &Json, format: Output) -> String {
    match format {
        Output::Json => serde_json::to_string_pretty(value).unwrap(),
        Output::Text | Output::Table => {
            if let Some(text) = value.get("Sdl").or_else(|| value.get("Sql")) {
                return text.as_str().unwrap().trim_end().into();
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
            [
                "schema",
                "query",
                "events",
                "expand-sdl",
                "import-openapi",
                "compile-sql",
                "generate-migration",
            ]
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
    let format = if matches!(
        &cli.command,
        Command::Schema {
            command: SchemaCommand::ImportOpenapi { .. }
        }
    ) {
        Output::Text
    } else {
        cli.output
    };
    match execute(cli.command) {
        Ok(value) => {
            if writeln!(io::stdout().lock(), "{}", output(&value, format)).is_err() {
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
            ExitCode::from(if matches!(error, Error::OpenApi(_)) {
                2
            } else {
                254
            })
        }
    }
}
